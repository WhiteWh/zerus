#!/usr/bin/python3
"""Built-app cleanup/recovery walk, private D-Bus/Xvfb and fake hgs only.

After npm run build, build the app with --features media,tauri/custom-protocol.
The default development-mode binary expects a separately running dev server.
"""
import argparse, ctypes, json, os, re, signal, subprocess, sys, tempfile, time
from pathlib import Path
REPO = Path(__file__).resolve().parents[2]
MACHINE = '11111111-1111-4111-8111-111111111111'
if '--worker' not in sys.argv:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--recovery', action='store_true')
    parser.add_argument('--legacy', action='store_true', help='Recovery against help without guarded-create-v1')
    parser.add_argument('--undo-native', action='store_true', help='Native delay/cancel/hidden/source/quit seam')
    parser.add_argument('--undo-ui', action='store_true', help='Visible Undo, five windows, exact batch and close-to-tray')
    args = parser.parse_args()
    if args.legacy or args.undo_native or args.undo_ui: args.recovery = True
    app = args.app.resolve()
    assert app.is_file()
    root = Path(tempfile.mkdtemp(prefix='zerus-cleanup-walk-'))
    for name in ('home', 'config', 'data', 'cache', 'runtime', 'tmp', 'tmux', 'state', 'bin', 'repo'):
        (root / name).mkdir(mode=0o700)
    (root/'repo/.git').mkdir()
    tags = ('kept', 'unowned', 'unknown', 'restore-refused', 'changed', *(f'extra-{n}' for n in range(61))) if args.recovery else ('clean', 'kept', 'unknown', 'unknown-only', 'changed')
    if args.recovery: (root/'recovery').write_text(json.dumps(tags))
    if args.legacy: (root/'legacy').touch()
    if args.undo_native or args.undo_ui: (root/'undo-native').touch()
    if args.undo_ui: (root/'undo-ui').touch()
    rows = []
    for tag in tags:
        folder = root/'repo'/tag
        folder.mkdir()
        (folder/'changes.txt').write_text('Synthetic local changes only.\n')
        rows.append({'key': 'run:'+tag, 'row': {'name': 'codex/example/'+tag, 'cmd': 'codex', 'agent': 'codex', 'project': 'example', 'tag': tag, 'run_id': 'run-'+tag, 'conversation_id': 'conv-'+tag, 'tracked': True, 'resumable': True, 'state': 'running', 'runtime_state': 'live', 'process_state': 'running', 'phase': 'idle', 'activity': 'idle', 'cwd': str(folder), 'git_root': str(folder), 'git_common_dir': str(root/'repo/.git'), 'git_worktree': True, 'git_branch': tag, 'worktree_owner_id': 'owner-'+tag}})
        if args.recovery: rows[-1]['row'].update(state='paused',runtime_state='paused',process_state='stopped')
    frames = [
        {'v':1,'seq':1,'t':'hello','machine_id':MACHINE,'host':'example-host','hgs_version':'1.56.0','stream':'synthetic-cleanup','heartbeat_ms':10000,'max_frame_bytes':1048576,'topics':['catalog','catalog.meta'],'limits':{'activity_subscriptions':16,'page_bytes':524288,'queue_bytes':4194304,'activity_tail':100,'activity_read':200,'control_line_bytes':65536,'git_subscriptions':64,'processes_subscriptions':16}},
        {'v':1,'seq':2,'t':'snapshot','topic':'catalog','items':rows,'page':0,'last':True,'cursor':1},
        {'v':1,'seq':3,'t':'snapshot','topic':'catalog.meta','value':{'host':'example-host','ok':True,'hgs_version':'1.56.0','projects':{},'peers':[]},'page':0,'last':True,'cursor':1},
        {'v':1,'seq':4,'t':'heartbeat','at':1793000000.0},
    ]
    (root/'replay.ndjson').write_text(''.join(json.dumps(f)+'\n' for f in frames))
    fake = root/'bin/fake-hgs'
    fake.write_text('''#!/usr/bin/python3
import json,sys,time
from pathlib import Path
root=Path(__file__).resolve().parent.parent
args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as f:f.write(json.dumps(args)+'\\n')
with (root/'calls-time.jsonl').open('a') as f:f.write(json.dumps({'at':time.monotonic(),'args':args})+'\\n')
if args==['--help']:
 if (root/'legacy').exists():print('hgs stable help');sys.exit(0)
 print('guarded-create-v1\\n hgs [@host] session-action <session> --json scoped native lifecycle action');sys.exit(0)
guarded=args[:2]==['--expect-machine','11111111-1111-4111-8111-111111111111']
if guarded:args=args[2:]
else:assert ((root/'legacy').exists() and args[0]=='archive') or ((root/'recovery').exists() and args[0]=='worktrees' and len(args)>1 and args[1].startswith('--')) or (args[0]=='inspect' and '--after' in args and args[args.index('--after')+1]==str(2**63-1) and args[1].startswith('codex/example/')),args
def arg(key):return args[args.index(key)+1]
if (root/'recovery').exists():
 import uuid
 def archive_id(tag):return str(uuid.uuid5(uuid.NAMESPACE_URL,'https://example.test/archive/'+tag))
 def review_row(tag):
  return {'path':str(root/'repo'/tag),'kind':'linked','available':True,'branch':tag,'head':'head-'+tag,'merged':True,
   'verdict':'blocked','fingerprint':'fp-'+tag,'reasons':[{'code':'processes_unavailable','message':'Private detail /private/secret'}],
   'ownership_id':'owner-'+tag,'zerus_owned':True,'archive_restore_ready':True,'changes':2,'untracked':1,'ignored_count':1,
   'sessions':['synthetic/other'],'process_count':2,'processes':[{'pid':1},{'pid':2}],'unreadable_processes':[{'pid':3}],
   'disk_bytes':1024,'disk_complete':True,'integration':{'kind':'ancestor','base':'main'}}
 if args[0]=='archive' or (args[0]=='session-action' and (root/'undo-native').exists()):
  scoped=None
  if args[0]=='session-action':
   scoped=json.load(sys.stdin);assert scoped['action']=='archive'
  name=args[1];tag=name.rsplit('/',1)[-1];(root/('archived-'+tag)).write_text(archive_id(tag))
  report={'status':'unconfirmed' if tag in ('unknown','extra-60') else 'kept','path':str(root/'repo'/tag),'common_dir':str(root/'repo/.git'),
   'reason':'Cannot remove private paths: '+', '.join('/private/secret-'+str(n) for n in range(30)),
   'archive':{'version':1,'archive_id':archive_id(tag),'name':name,'run_id':'run-'+tag,'conversation_id':'conv-'+tag,'owner_id':'owner-'+tag,'launch_owned':tag!='unowned'}}
  if tag!='unowned':report['review']=review_row(tag)
  print('ZERUS_WORKTREE_CLEANUP '+json.dumps(report),file=sys.stderr)
  if scoped:
   assert scoped['expected_run_id']=='run-'+tag and scoped['expected_conversation_id']=='conv-'+tag
   print(json.dumps({'request_id':scoped['request_id'],'name':name,'run_id':'run-'+tag,'conversation_id':'conv-'+tag,'status':'completed'}))
  else:print('hgs: archived '+name)
  sys.exit(0)
 if args[0]=='inspect' and (root/'undo-native').exists():
  name=args[1];tag=name.rsplit('/',1)[-1];assert arg('--after')==str(2**63-1)
  print(json.dumps({'name':name,'run_id':'replacement' if tag=='changed' else 'run-'+tag,'conversation_id':'conv-'+tag,'state':'paused',
   'current_provider_detail':'x'*(3*1024*1024 if tag=='restore-refused' else 256*1024)}));sys.exit(0)
 if args[0]=='inspect' and '--archive' in args:
  name=args[1];tag=name.rsplit('/',1)[-1];assert arg('--archive')==archive_id(tag)
  assert (root/('archived-'+tag)).exists() and not (root/('restored-'+tag)).exists()
  assert arg('--after')==str(2**63-1),'restore metadata read must not read the journal'
  print(json.dumps({'state':'archived','name':name,'run_id':'replacement' if tag=='changed' else 'run-'+tag,'conversation_id':'conv-'+tag,'archive_id':archive_id(tag)}));sys.exit(0)
 if args[0]=='resume':
  name=args[1];tag=name.rsplit('/',1)[-1];assert arg('--archive')==archive_id(tag);assert '-d' in args
  assert tag!='changed','changed source must never reach restore'
  if tag=='restore-refused':print('The archived conversation changed; open Archive and refresh.',file=sys.stderr);sys.exit(1)
  time.sleep(.4);(root/('restored-'+tag)).write_text('one exact synthetic restore');print('hgs: restored '+name);sys.exit(0)
 if args[0]=='worktrees' and (len(args)<2 or args[1].startswith('--')):
  assert Path(arg('--path')).is_relative_to(root)
  tags=json.loads((root/'recovery').read_text());print(json.dumps({'state':'ok','path':arg('--path'),'selected_root':str(root/'repo'),'common_dir':str(root/'repo/.git'),'sampled_at':time.time(),
   'worktrees':[{'path':str(root/'repo'),'kind':'main','available':True,'branch':'main'},*[review_row(tag) for tag in tags]]}));sys.exit(0)
 if args[:2]==['worktrees','review']:
  tags=json.loads((root/'recovery').read_text())
  if '--worktree' in args:tags=[tag for tag in tags if str(root/'repo'/tag)==arg('--worktree')]
  print(json.dumps({'state':'ok','common_dir':str(root/'repo/.git'),'worktrees':[review_row(tag) for tag in tags]}));sys.exit(0)
if args[0]=='inspect':
 name=args[1];tag=name.rsplit('/',1)[-1];counter=root/('inspect-'+tag)
 n=int(counter.read_text())+1 if counter.exists() else 1;counter.write_text(str(n))
 print(json.dumps({'name':name,'run_id':'run-'+tag,'conversation_id':'conv-'+tag,'tracked':True,'state':'running','cmd':'codex','git_worktree':True,'git_root':str(root/'repo'/tag),'git_common_dir':str(root/'repo/.git'),'worktree_owner_id':'replacement' if tag=='changed' and n>1 else 'owner-'+tag}));sys.exit(0)
if args[0]=='session-action':
 name=args[1];tag=name.rsplit('/',1)[-1];data=json.load(sys.stdin)
 assert data['action']=='terminate' and data['expected_run_id']=='run-'+tag and data['expected_conversation_id']=='conv-'+tag
 assert tag!='changed','changed source must never reach mutation'
 print(json.dumps({'request_id':data['request_id'],'name':name,'run_id':'run-'+tag,'conversation_id':'conv-'+tag,'status':'completed','result_target':{'name':name,'run_id':'run-'+tag,'conversation_id':'conv-'+tag,'archive_id':'22222222-2222-4222-8222-222222222222'}}));sys.exit(0)
if args[:2]==['worktrees','review']:
 path=Path(arg('--worktree'));tag=path.name;common=arg('--path');assert path.is_relative_to(root)
 if tag=='clean':time.sleep(2)
 row={'path':str(path),'kind':'linked','branch':tag,'zerus_owned':True,'ownership_id':'owner-'+tag,'archive_restore_ready':True,'verdict':'ready','fingerprint':'fingerprint-'+tag,'reasons':[],'changes':0,'untracked':0,'disk_complete':True,'ignored_complete':True,'ignored_count':0,'integration':{'kind':'ancestor'},'disk_bytes':1200}
 if tag=='kept':row.update({'verdict':'blocked','changes':1,'discardable':True,'discard_fingerprint':'discard-kept','reasons':[{'code':'changes','message':'Uncommitted changes need review.'}]})
 print(json.dumps({'state':'ok','common_dir':common,'worktrees':[row]}));sys.exit(0)
if args[:2]==['worktrees','remove']:
 path=Path(arg('--path'));tag=path.name;assert path.is_relative_to(root)
 assert '--delete-branch' not in args
 assert '--preserve-archive-restore' in args
 assert '--process-fingerprint' not in args
 if tag=='kept':assert arg('--discard-fingerprint')=='discard-kept'
 if tag.startswith('unknown'):print('Synthetic unknown after issue',file=sys.stderr);print('{}');sys.exit(1)
 print(json.dumps({'request_id':arg('--request-id'),'path':str(path),'status':'removed'}));sys.exit(0)
if args[:2]==['worktrees','inspect']:
 path=arg('--path');common=arg('--common-dir');assert Path(path).is_relative_to(root)
 data={'state':'ok','path':path,'common_dir':common,'files':[{'path':'changes.txt','status':' M'}],'changes':{'text':'Synthetic read-only diff'},'commits':{'text':'No additional commits.'},'review':{'sessions':[]}}
 if '--file' in args:data['file']={'path':arg('--file'),'entries':[{'path':'changes.txt','directory':False,'symlink':False}],'offset':0}
 print(json.dumps(data));sys.exit(0)
raise RuntimeError(args)
''')
    fake.chmod(0o700)
    (root/'bus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir='+str(root/'runtime')+'</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
    env = {'PATH':str(root/'bin')+':/usr/bin:/bin','LANG':'C.UTF-8','HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'config'),'XDG_DATA_HOME':str(root/'data'),'XDG_CACHE_HOME':str(root/'cache'),'XDG_RUNTIME_HOME':str(root/'runtime'),'TMPDIR':str(root/'tmp'),'TMUX_TMPDIR':str(root/'tmux'),'HGS_CONFIG_DIR':str(root/'config'),'HGS_STATE_DIR':str(root/'state'),'GDK_BACKEND':'x11','WAYLAND_DISPLAY':'zerus-nonexistent','WEBKIT_DISABLE_DMABUF_RENDERER':'1','NO_AT_BRIDGE':'1','GSETTINGS_BACKEND':'memory','ZERUS_CLIENT_HEADLESS':'1','ZERUS_CLIENT_REPLAY':str(root/'replay.ndjson'),'ZERUS_CLIENT_HGS':str(fake),'ZERUS_CLIENT_QUERY':'page=sessions&skin=zerus-light&termination-cleanup-smoke=1','ZERUS_CLIENT_PERF':'smoke'}
    env['XDG_RUNTIME_DIR']=str(root/'runtime');env['XDG_STATE_HOME']=str(root/'state')
    if args.recovery:env['ZERUS_CLIENT_QUERY']='page=sessions&skin=zerus-light&archive-cleanup-smoke=1'
    if args.legacy:env['ZERUS_CLIENT_QUERY']='page=sessions&skin=zerus-light&archive-cleanup-smoke=legacy'
    if args.undo_native:env['ZERUS_CLIENT_QUERY']='page=sessions&skin=zerus-light&end-action-native-smoke=1'
    if args.undo_ui:env['ZERUS_CLIENT_QUERY']='page=sessions&skin=zerus-light&end-action-ui-smoke=1'
    (root/'environment.json').write_text(json.dumps(env))
    print('Private Archive recovery walk:' if args.recovery else 'Private Terminate cleanup walk:', root, flush=True)
    child = subprocess.Popen(['/usr/bin/dbus-run-session','--config-file',str(root/'bus.conf'),'--','/usr/bin/xvfb-run','-a','-s','-screen 0 1440x900x24','/usr/bin/python3',str(Path(__file__).resolve()),'--worker',str(root),str(app),*(['recovery'] if args.recovery else [])],env=env,start_new_session=True)
    try: result=child.wait(timeout=95)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid,signal.SIGTERM);child.wait(timeout=5);raise
    raise SystemExit(result)
root, app = Path(sys.argv[2]), Path(sys.argv[3])
recovery=sys.argv[4:]==['recovery']
undo_native=(root/'undo-native').exists()
undo_ui=(root/'undo-ui').exists()
assert os.environ['HOME']==str(root/'home')
# Private wrapper-generated X11 and D-Bus only; no owner environment inherited.
env=json.loads((root/'environment.json').read_text())
assert re.fullmatch(r':[0-9]+(?:\.0)?',os.environ['DISPLAY'])
assert str(root/'runtime') in os.environ['DBUS_SESSION_BUS_ADDRESS']
assert Path(os.environ['XAUTHORITY']).resolve().is_relative_to(root/'tmp')
env.update({key:os.environ[key] for key in ('DISPLAY','DBUS_SESSION_BUS_ADDRESS','XAUTHORITY')})
x=ctypes.CDLL('libX11.so.6');t=ctypes.CDLL('libXtst.so.6')
x.XOpenDisplay.argtypes=[ctypes.c_char_p];x.XOpenDisplay.restype=ctypes.c_void_p
x.XStringToKeysym.argtypes=[ctypes.c_char_p];x.XStringToKeysym.restype=ctypes.c_ulong
x.XKeysymToKeycode.argtypes=[ctypes.c_void_p,ctypes.c_ulong];x.XKeysymToKeycode.restype=ctypes.c_uint
x.XFlush.argtypes=[ctypes.c_void_p];t.XTestFakeKeyEvent.argtypes=[ctypes.c_void_p,ctypes.c_uint,ctypes.c_int,ctypes.c_ulong]
x.XSetInputFocus.argtypes=[ctypes.c_void_p,ctypes.c_ulong,ctypes.c_int,ctypes.c_ulong]
display=x.XOpenDisplay(None);assert display
with (root/'app.log').open('wb') as log:
    child=subprocess.Popen([str(app)],env=env,stdout=log,stderr=log,cwd=REPO/'client')
    stages=set();deadline=time.monotonic()+85
    try:
        while time.monotonic()<deadline:
            output=(root/'app.log').read_text()
            prefix='undo-ui: ' if undo_ui else 'undo-native: ' if undo_native else 'archive-smoke: ' if recovery else 'cleanup-smoke: '
            for stage in (('UNDO','FIVE','CLOSE','RESTORED','BATCH') if undo_ui else ('NATIVE','RESTORED','SOURCE_CHANGED') if undo_native else ('NOTICE','REVIEW','RESTORING','RESTORED','REFUSED','UNOWNED','UNKNOWN','OVERFLOW') if recovery else ('CLEANING','MIXED','REOPENED','UNKNOWN','INSPECTION')):
                if prefix+stage in output and stage not in stages:
                    if (undo_ui and stage=='CLOSE') or (not undo_ui and undo_native and stage=='NATIVE'):(root/'pending-observed').write_text(str(time.monotonic()))
                    stages.add(stage);time.sleep(.15)
                    subprocess.run(['/usr/bin/import','-window','root',str(root/(stage.lower()+'.png'))],check=True,env=env)
            if ('archive-smoke: ESCAPE_REVIEW' if recovery else 'cleanup-smoke: ESCAPE_CLEANING') in output and 'escape' not in stages:
                tree=subprocess.check_output(['/usr/bin/xwininfo','-root','-tree'],env=env,text=True)
                windows=re.findall(r'(0x[0-9a-f]+) "Zerus(?: Next)?"',tree)
                assert windows,'private main window is missing'
                window=next((w for w in windows if 'IsViewable' in subprocess.check_output(['/usr/bin/xwininfo','-id',w],env=env,text=True)),None)
                assert window,'private main window must be IsViewable before key input'
                x.XSetInputFocus(display,int(window,16),2,0);x.XFlush(display)
                code=x.XKeysymToKeycode(display,x.XStringToKeysym(b'Escape'))
                t.XTestFakeKeyEvent(display,code,1,0);t.XTestFakeKeyEvent(display,code,0,0);x.XFlush(display);stages.add('escape')
            if child.poll() is not None: break
            time.sleep(.03)
        else: raise RuntimeError('private built-app walk timed out')
        print('\n'.join(l for l in output.splitlines() if ('undo-ui:' if undo_ui else 'undo-native:' if undo_native else 'archive-smoke:' if recovery else 'cleanup-smoke:') in l))
        assert child.returncode==0, output
        calls=[json.loads(l) for l in (root/'calls.jsonl').read_text().splitlines()]
        effects=[c for c in calls if 'session-action' in c]
        removals=[c for c in calls if 'remove' in c]
        if undo_ui:
            assert len(effects)==4,effects
            assert [c[-2] for c in effects]==['codex/example/kept',*[f'codex/example/extra-{n}' for n in (6,7,8)]],effects
            assert not removals
            issued=next(c for c in map(json.loads,(root/'calls-time.jsonl').read_text().splitlines()) if 'session-action' in c['args'])
            assert issued['at']-float((root/'pending-observed').read_text())>=4.7,'effect before grace'
            assert {'UNDO','FIVE','CLOSE','RESTORED','BATCH'}<=stages,stages
            assert 'undo-ui: EXECUTED_CLOSED' in output and 'undo-ui: QUIT_PENDING' in output,output
            assert 'dropped pending Archive for "codex/example/extra-9" on quit' in output,output
            print('PASS visible Undo/focus/five/sixth/whole batch/exact dispatch/close-to-tray/changed/quit; four scoped fake archives only')
            raise SystemExit(0)
        if undo_native:
            assert len(effects)==1 and effects[0][-2:]==['codex/example/kept','--json'],effects
            assert not removals and not any('codex/example/extra-0' in c for c in calls),'quit started a pending action'
            assert sum('codex/example/changed' in c and 'inspect' in c for c in calls)==1,calls
            assert sum('codex/example/restore-refused' in c and 'inspect' in c for c in calls)==1,calls
            issued=next(c for c in map(json.loads,(root/'calls-time.jsonl').read_text().splitlines()) if 'session-action' in c['args'])
            assert issued['at']-float((root/'pending-observed').read_text())>=4.7,'effect issued before the native window'
            assert all('undo-native: '+s in output for s in ('EXECUTED_HIDDEN','SOURCE_CHANGED','LIMIT_REFUSED','QUIT_PENDING')),output
            assert {'NATIVE','RESTORED'}<=stages,stages
            assert all((root/'repo'/tag/'changes.txt').exists() for tag in json.loads((root/'recovery').read_text()))
            print('PASS native five-second/cancel/hidden/exact source/quit seam; one scoped fake archive only')
            raise SystemExit(0)
        if recovery:
            archives=[c for c in calls if 'archive' in c];restores=[c for c in calls if 'resume' in c]
            if (root/'legacy').exists():
                assert len(archives)==1 and '--expect-machine' not in archives[0],archives
                assert not restores and not removals and not effects,'legacy recovery must not run Restore or cleanup'
                assert {'NOTICE','REVIEW','escape'}<=stages,stages
                assert 'archive-smoke: LEGACY_PASS' in output,output
                assert all((root/'repo'/tag/'changes.txt').exists() for tag in json.loads((root/'recovery').read_text()))
                print('PASS legacy Archive, retained Review only, singular copy and Escape focus; no Restore or cleanup')
                raise SystemExit(0)
            assert len(archives)==66,len(archives)
            assert len(restores)==3,restores
            assert not removals and not effects,'recovery must not replay cleanup or terminate'
            assert all('--expect-machine' in c for c in archives+restores)
            assert {'NOTICE','REVIEW','RESTORING','RESTORED','REFUSED','UNOWNED','UNKNOWN','OVERFLOW'}<=stages,stages
            assert (root/'restored-kept').exists() and (root/'restored-extra-60').exists()
            assert not (root/'restored-changed').exists() and not (root/'restored-restore-refused').exists()
            assert all((root/'repo'/tag/'changes.txt').exists() for tag in json.loads((root/'recovery').read_text()))
            print('PASS bounded archive notices, exact guarded restore, refusal, source change, unknown non-replay, overflow and Escape focus; fake hgs only')
            raise SystemExit(0)
        assert len(effects)==4,effects
        assert len(removals)==4,removals
        assert len([c for c in removals if c[c.index('--path')+1].endswith('/unknown')])==1,'unknown was repeated'
        assert all('--expect-machine' in c for c in effects+removals)
        assert all((root/'repo'/tag/'changes.txt').exists() for tag in ('clean','kept','unknown','unknown-only','changed')),'fake hgs must not delete even fixture files'
        assert {'CLEANING','MIXED','REOPENED','UNKNOWN','INSPECTION'}<=stages
        print('PASS exact scoped fake termination; generated cleanup; explicit discard; unknown non-replay; source change refusal; chip, review, inspection, Escape and Keep; no real removal')
    finally:
        if child.poll() is None: child.terminate();child.wait(timeout=5)
