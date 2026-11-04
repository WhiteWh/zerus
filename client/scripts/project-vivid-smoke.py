#!/usr/bin/python3
"""Two built-app launches, private state/bus/Xvfb and one fake color write."""
import argparse, ctypes, json, os, signal, subprocess, sys, tempfile, time
from pathlib import Path
REPO = Path(__file__).resolve().parents[2]
if '--worker' not in sys.argv:
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--app',type=Path,required=True);parser.add_argument('--default',action='store_true');parser.add_argument('--folders',action='store_true');parser.add_argument('--clone',action='store_true');args=parser.parse_args();app=args.app.resolve();assert app.is_file()
    root=Path(tempfile.mkdtemp(prefix='zerus-project-vivid-'))
    for name in ('home','config','data','cache','runtime','tmp','tmux','state','bin'):(root/name).mkdir(mode=0o700)
    if args.default:(root/'default').touch()
    if args.folders:(root/'folders').touch()
    if args.clone:(root/'clone').touch()
    frames=[json.loads(line) for line in (REPO/'client/fixtures/replay.ndjson').read_text().splitlines()]
    replay_frames=json.loads(json.dumps(frames))
    parts={i['key']:i['row'] for f in frames if f.get('topic')=='organization' for i in f.get('items',[])}
    snapshot=parts['organization'];ids=snapshot['organization']['projects'];projects=[]
    for key in ids:
        project=dict(parts['project:'+key]);chunks=project.pop('session_chunks',0)
        project['sessions']=[s for n in range(chunks) for s in parts[f'members:{key}:{n}']['sessions']];projects.append(project)
    snapshot['organization']['projects']=projects;snapshot['versions']={key[len('version:'):]:value for key,value in parts.items() if key.startswith('version:')}
    if args.folders:
        for tag in ('a','b','c'):
            (root/('folder-'+tag)).mkdir();(root/('folder-'+tag)/'keep.txt').write_text('Synthetic private data stays in place.\n')
        source=[{'id':'folder-a','machine':'workstation','path':str(root/'folder-a'),'name':'','private_extension':{'retain':True}}, {'id':'folder-b','machine':'workstation','path':str(root/'folder-b'),'name':'Second folder','machine_id':'other-physical-computer'}]
        next(p for p in projects if p['id']=='p-sample-project')['folders']=source
        for f in replay_frames:
            if f.get('topic')=='organization':
                for item in f.get('items',[]):
                    if item['key']=='project:p-sample-project':item['row']['folders']=source
        (root/'replay.ndjson').write_text(''.join(json.dumps(f)+'\n' for f in replay_frames))
        opener=root/'bin/xdg-open';opener.write_text('#!/usr/bin/python3\nimport sys,json\nfrom pathlib import Path\nroot=Path(__file__).resolve().parent.parent\nwith (root/"opened.jsonl").open("a") as f:f.write(json.dumps(sys.argv[1:])+"\\n")\n');opener.chmod(0o700)
    (root/'snapshot.json').write_text(json.dumps(snapshot))
    fake=root/'bin/fake-hgs';fake.write_text('''#!/usr/bin/python3
import json,sys
from pathlib import Path
root=Path(__file__).resolve().parent.parent
args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as f:f.write(json.dumps(args)+'\\n')
if (root/'clone').exists() and args[:1]==['--expect-machine']:
 assert args[1]=='00000000-0000-4000-8000-000000000001';args=args[2:]
 if args==['--help']:print('repository-clone-v1');sys.exit(0)
 assert args[:1]==['repository'];op=args[1]
 if op=='check':
  parent=args[args.index('--parent')+1];name=args[args.index('--name')+1]
  print(json.dumps(dict(status='ready',parent=parent,name=name,path=parent+'/'+name)));sys.exit(0)
 assert op in ('clone','status')
 request=args[args.index('--request-id')+1];saved=json.loads((root/'data/dev.zerus.next/settings-replay.json').read_text())
 record=next(v['pending'] for v in saved['repository_clones']['entries'].values() if v.get('pending') and v['pending']['request_id']==request)
 print(json.dumps(dict(record,status='cloned' if (root/'restored').exists() else 'unconfirmed')));sys.exit(0)
if args==['swarm','apply']:
 req=json.load(sys.stdin)
 with (root/'applies.jsonl').open('a') as f:f.write(json.dumps(req)+'\\n')
 value=json.loads((root/'snapshot.json').read_text());value['organization']=req['desired'];value['digest']='color-1'
 token=json.dumps({'kind':'project','id':'p-sample-project','field':'color'},separators=(',',':'))
 value['versions'][token]=['color-op'];value['written']={token:['color-op']};
 if (root/'folders').exists() or (root/'clone').exists():
  sequence=len((root/'applies.jsonl').read_text().splitlines());value['digest']='folders-'+str(sequence);value['written']={}
  for project in req['desired']['projects']:
   token=json.dumps({'kind':'project','id':project['id'],'field':'folders'},separators=(',',':'));value['versions'][token]=['folder-op-'+str(sequence)];value['written'][token]=['folder-op-'+str(sequence)]
 (root/'snapshot.json').write_text(json.dumps(value));print(json.dumps(value));sys.exit(0)
if args==['swarm','get']:print((root/'snapshot.json').read_text());sys.exit(0)
print('{}');sys.exit(1)
''');fake.chmod(0o700)
    (root/'bus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir='+str(root/'runtime')+'</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
    env={'PATH':str(root/'bin')+':/usr/bin:/bin','LANG':'C.UTF-8','HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'config'),'XDG_DATA_HOME':str(root/'data'),'XDG_CACHE_HOME':str(root/'cache'),'XDG_RUNTIME_HOME':str(root/'runtime'),'TMPDIR':str(root/'tmp'),'TMUX_TMPDIR':str(root/'tmux'),'HGS_CONFIG_DIR':str(root/'config'),'HGS_STATE_DIR':str(root/'state'),'GDK_BACKEND':'x11','WAYLAND_DISPLAY':'zerus-nonexistent','WEBKIT_DISABLE_DMABUF_RENDERER':'1','NO_AT_BRIDGE':'1','GSETTINGS_BACKEND':'memory','ZERUS_CLIENT_HEADLESS':'1','ZERUS_CLIENT_REPLAY':str(REPO/'client/fixtures/replay.ndjson'),'ZERUS_CLIENT_HGS':str(fake),'ZERUS_CLIENT_PERF':'smoke'}
    if args.folders:env['ZERUS_CLIENT_REPLAY']=str(root/'replay.ndjson')
    print('Private project vivid walk:',root,flush=True)
    child=subprocess.Popen(['/usr/bin/dbus-run-session','--config-file',str(root/'bus.conf'),'--','/usr/bin/xvfb-run','-a','-s','-screen 0 1440x900x24','/usr/bin/python3',str(Path(__file__).resolve()),'--worker',str(root),str(app)],env=env,start_new_session=True)
    try:result=child.wait(timeout=100)
    except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGTERM);child.wait(timeout=5);raise
    raise SystemExit(result)
root,app=Path(sys.argv[2]),Path(sys.argv[3]);default_mode=(root/'default').exists();folder_mode=(root/'folders').exists();clone_mode=(root/'clone').exists();assert os.environ['HOME']==str(root/'home')
env={k:v for k,v in os.environ.items() if k in {'PATH','LANG','HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','XDG_RUNTIME_HOME','TMPDIR','TMUX_TMPDIR','HGS_CONFIG_DIR','HGS_STATE_DIR','GDK_BACKEND','WAYLAND_DISPLAY','WEBKIT_DISABLE_DMABUF_RENDERER','NO_AT_BRIDGE','GSETTINGS_BACKEND','DISPLAY','DBUS_SESSION_BUS_ADDRESS','XAUTHORITY'} or k.startswith('ZERUS_CLIENT_')}
x=ctypes.CDLL('libX11.so.6');t=ctypes.CDLL('libXtst.so.6');x.XOpenDisplay.argtypes=[ctypes.c_char_p];x.XOpenDisplay.restype=ctypes.c_void_p;x.XStringToKeysym.argtypes=[ctypes.c_char_p];x.XStringToKeysym.restype=ctypes.c_ulong;x.XKeysymToKeycode.argtypes=[ctypes.c_void_p,ctypes.c_ulong];x.XKeysymToKeycode.restype=ctypes.c_uint;x.XFlush.argtypes=[ctypes.c_void_p];t.XTestFakeKeyEvent.argtypes=[ctypes.c_void_p,ctypes.c_uint,ctypes.c_int,ctypes.c_ulong];display=x.XOpenDisplay(None);assert display
for mode in (('first',) if folder_mode else ('first','restored')):
    if clone_mode and mode=='restored':(root/'restored').touch()
    case_env=dict(env);case_env['ZERUS_CLIENT_QUERY']='page=projects&skin=zerus-light&'+('project-clone-smoke' if clone_mode else 'project-folder-smoke' if folder_mode else 'project-default-smoke' if default_mode else 'project-vivid-smoke')+'='+mode
    prefix='project-clone-smoke: ' if clone_mode else 'project-folder-smoke: ' if folder_mode else 'project-default-smoke: ' if default_mode else 'project-vivid-smoke: '
    path=root/(mode+'.log')
    with path.open('wb') as log:
        child=subprocess.Popen([str(app)],cwd=REPO/'client',env=case_env,stdout=log,stderr=log);stages=set();deadline=time.monotonic()+40
        try:
            while time.monotonic()<deadline:
                output=path.read_text()
                if clone_mode and 'initial' not in stages and time.monotonic()>deadline-30:
                    subprocess.run(['/usr/bin/import','-window','root',str(root/'initial.png')],check=True,env=case_env);stages.add('initial')
                for stage in (('CHOOSER','FORM','UNKNOWN','REOPENED','RESTORED','READY','ADDED','NEW_PROJECT') if clone_mode else ('EDIT','DUPLICATE','SAVED','COPIED','MOVED','ADDED') if folder_mode else ('CHOSEN','RESTORED') if default_mode else ('SOFT','BRIGHT','SAVED','RESTORED')):
                    if prefix+stage in output and stage not in stages:
                        stages.add(stage);time.sleep(.15);subprocess.run(['/usr/bin/import','-window','root',str(root/(stage.lower()+'.png'))],check=True,env=case_env)
                if 'project-vivid-smoke: ESCAPE' in output and 'escape' not in stages:
                    key=x.XKeysymToKeycode(display,x.XStringToKeysym(b'Escape'));t.XTestFakeKeyEvent(display,key,1,0);t.XTestFakeKeyEvent(display,key,0,0);x.XFlush(display);stages.add('escape')
                if child.poll() is not None:break
                time.sleep(.03)
            else:raise RuntimeError('project vivid walk timed out')
            print('\n'.join(l for l in output.splitlines() if prefix in l));assert child.returncode==0,output
        finally:
            if child.poll() is None:child.terminate();child.wait(timeout=5)
    if clone_mode or folder_mode:continue
    if default_mode:
        saved=json.loads((root/'data/dev.zerus.next/organization-layout-replay.json').read_text());assert saved['default_project']=='p-sample-project'
        continue
    saved=json.loads((root/'data/dev.zerus.next/settings-replay.json').read_text())
    assert saved['values']['projects/vivid/p-sample-project'] is False
if clone_mode:
    calls=[json.loads(line) for line in (root/'calls.jsonl').read_text().splitlines()]
    clone=[a for a in calls if 'repository' in a and 'clone' in a];status=[a for a in calls if 'repository' in a and 'status' in a]
    assert len(clone)==2 and len(status)==1,(clone,status)
    assert clone[0][clone[0].index('--request-id')+1]==status[0][status[0].index('--request-id')+1]
    requests=[json.loads(line) for line in (root/'applies.jsonl').read_text().splitlines()];assert len(requests)==2,len(requests)
    assert not any(any(w in a for w in ('new','start','create','resume','send')) for a in calls),calls
    print('PASS durable clone before delivery, unknown close/reopen/restart without replay, exact status, existing/new project placement; no agent or worktree creation')
    raise SystemExit(0)
if folder_mode:
    requests=[json.loads(line) for line in (root/'applies.jsonl').read_text().splitlines()];assert len(requests)==5,len(requests)
    value=json.loads((root/'snapshot.json').read_text())['organization'];a=next(p for p in value['projects'] if p['id']=='p-sample-project')['folders'];b=next(p for p in value['projects'] if p['id']=='p-aurora')['folders']
    assert len(a)==1 and a[0]['path']==str(root/'folder-c') and a[0]['name']=='New folder'
    assert len(b)==1 and b[0]['path']==str(root/'folder-a') and b[0]['name']=='Updated source' and b[0]['id']!='folder-a' and b[0]['private_extension']=={'retain':True}
    assert all((root/('folder-'+tag)/'keep.txt').read_text()=='Synthetic private data stays in place.\n' for tag in ('a','b','c'))
    opened=[json.loads(line) for line in (root/'opened.jsonl').read_text().splitlines()];assert opened==[[str(root/'folder-a')]],opened
    print('PASS exact folder edit/duplicate/cancel, shared copy/move dedup/remove/add, one local open, stale source/owner refusal; private files unchanged')
    raise SystemExit(0)
if default_mode:
    assert not (root/'applies.jsonl').exists(),'local default must never send swarm apply'
    print('PASS default checkbox/publication/unchecked reversion, exact refusal, layout restart; zero shared writes')
    raise SystemExit(0)
applies=[json.loads(line) for line in (root/'applies.jsonl').read_text().splitlines()];assert len(applies)==1,applies
base,desired=applies[0]['base'],applies[0]['desired'];expected=json.loads(json.dumps(base))
next(p for p in expected['projects'] if p['id']=='p-sample-project')['color']='#ffcf33'
assert expected==desired,'color changed other shared fields'
assert not any('vivid' in key for key in desired), 'local style leaked into shared request'
print('PASS popup preview/Cancel/invalid-color, local exact-ID Apply, one existing shared color edit, false persisted across restart, Escape/focus; private state and fake hgs only')
