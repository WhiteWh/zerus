#!/usr/bin/python3
"""Video poster/render qualification in private D-Bus/Xvfb/state, fake hgs only."""
import argparse, base64, json, os, signal, subprocess, sys, tempfile, time
from pathlib import Path
REPO = Path(__file__).resolve().parents[2]
if '--worker' not in sys.argv:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    args = parser.parse_args(); app = args.app.resolve(); assert app.is_file()
    root = Path(tempfile.mkdtemp(prefix='zerus-video-poster-'))
    for name in ('home','config','data','cache','runtime','tmp','tmux','state','bin'):
        (root/name).mkdir(mode=0o700)
    env = {'PATH':str(root/'bin')+':/usr/bin:/bin','LANG':'C.UTF-8','HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'config'),'XDG_DATA_HOME':str(root/'data'),'XDG_CACHE_HOME':str(root/'cache'),'XDG_RUNTIME_HOME':str(root/'runtime'),'TMPDIR':str(root/'tmp'),'TMUX_TMPDIR':str(root/'tmux'),'HGS_CONFIG_DIR':str(root/'config'),'HGS_STATE_DIR':str(root/'state'),'GDK_BACKEND':'x11','WAYLAND_DISPLAY':'zerus-nonexistent','WEBKIT_DISABLE_DMABUF_RENDERER':'1','NO_AT_BRIDGE':'1','GSETTINGS_BACKEND':'memory'}
    subprocess.run(['/usr/bin/ffmpeg','-v','error','-f','lavfi','-i','color=c=teal:s=200x200:r=10','-t','1.2','-c:v','libx264','-pix_fmt','yuv420p','-threads','1','-movflags','+faststart',str(root/'video.mp4')],env=env,check=True,timeout=15)
    subprocess.run(['/usr/bin/ffmpeg','-v','error','-f','lavfi','-i','color=c=teal:s=200x200','-frames:v','1','-q:v','2','-threads','1',str(root/'poster.jpg')],env=env,check=True,timeout=15)
    poster=base64.b64encode((root/'poster.jpg').read_bytes()).decode(); assert (root/'poster.jpg').stat().st_size <= 8192
    events=[]
    for n in range(1,21):
        message='message-'+str(n)
        video={'request_id':message,'index':1,'format':'mp4','duration_ms':1200,'width':200,'height':200,'poster_base64':poster}
        events.append({'seq':n,'type':'UserMessage','source':'hgs_voice','message_id':message,'at':1793000000+n,'voice':{'id':message,'state':'submitted','audio':{'request_id':message,'index':0,'duration_ms':1200},'video':video}})
    frames=[
        {'v':1,'seq':1,'t':'hello','machine_id':'11111111-1111-4111-8111-111111111111','host':'example-host','hgs_version':'1.56.0','stream':'synthetic-video','heartbeat_ms':10000,'max_frame_bytes':1048576,'topics':['catalog','catalog.meta','activity'],'limits':{'activity_subscriptions':16,'page_bytes':524288,'queue_bytes':4194304,'activity_tail':100,'activity_read':200,'control_line_bytes':65536}},
        {'v':1,'seq':2,'t':'snapshot','topic':'catalog','items':[{'key':'run:video-one','row':{'name':'sh/example/video','cmd':'sh','agent':'sh','project':'example','tag':'video','run_id':'video-one','conversation_id':'video-conversation','cwd':'/synthetic/example','tracked':True,'resumable':True,'state':'running','runtime_state':'live','phase':'idle','activity':'idle'}}],'page':0,'last':True,'cursor':1},
        {'v':1,'seq':3,'t':'snapshot','topic':'catalog.meta','value':{'host':'example-host','ok':True,'hgs_version':'1.56.0','projects':{},'peers':[]},'page':0,'last':True,'cursor':1},
        {'v':1,'seq':4,'t':'events','topic':'activity','key':'run:video-one','events':events,'cursor':20,'has_older':False},
        {'v':1,'seq':5,'t':'state','topic':'activity','key':'run:video-one','rev':1,'value':{'name':'sh/example/video','run_id':'video-one','conversation_id':'video-conversation','phase':'idle'}},
        {'v':1,'seq':6,'t':'heartbeat','at':1793000100.0},
    ]
    (root/'replay.ndjson').write_text(''.join(json.dumps(f)+'\n' for f in frames))
    fake=root/'bin/fake-hgs'; fake.write_text('''#!/usr/bin/python3
import base64,json,sys
from pathlib import Path
root=Path(__file__).resolve().parent.parent;args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as f:f.write(json.dumps(args)+'\\n')
if args and args[0]=='attachment':
 assert args[1]=='sh/example/video' and args[args.index('--conversation')+1]=='video-conversation'
 request=args[args.index('--request')+1];index=int(args[args.index('--index')+1]);assert index==1
 print(json.dumps({'request_id':request,'index':index,'name':'video.mp4','mime':'video/mp4','data_base64':base64.b64encode((root/'video.mp4').read_bytes()).decode()}));sys.exit(0)
print('{}')
''');fake.chmod(0o700)
    env.update({'ZERUS_CLIENT_HEADLESS':'1','ZERUS_CLIENT_REPLAY':str(root/'replay.ndjson'),'ZERUS_CLIENT_HGS':str(fake),'ZERUS_CLIENT_QUERY':'page=sessions&skin=zerus-light&video-poster-smoke=1','ZERUS_CLIENT_PERF':'smoke'})
    (root/'bus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir='+str(root/'runtime')+'</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
    print('Private video poster walk:',root,flush=True)
    child=subprocess.Popen(['/usr/bin/dbus-run-session','--config-file',str(root/'bus.conf'),'--','/usr/bin/xvfb-run','-a','-s','-screen 0 1440x900x24','/usr/bin/python3',str(Path(__file__).resolve()),'--worker',str(root),str(app)],env=env,start_new_session=True)
    try: code=child.wait(timeout=110)
    except subprocess.TimeoutExpired: os.killpg(child.pid,signal.SIGTERM);child.wait(timeout=5);raise
    raise SystemExit(code)
root,app=Path(sys.argv[2]),Path(sys.argv[3]); assert os.environ['HOME']==str(root/'home')
env={k:v for k,v in os.environ.items() if k in {'PATH','LANG','HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','XDG_RUNTIME_HOME','TMPDIR','TMUX_TMPDIR','HGS_CONFIG_DIR','HGS_STATE_DIR','GDK_BACKEND','WAYLAND_DISPLAY','WEBKIT_DISABLE_DMABUF_RENDERER','NO_AT_BRIDGE','GSETTINGS_BACKEND','DISPLAY','DBUS_SESSION_BUS_ADDRESS','XAUTHORITY'} or k.startswith('ZERUS_CLIENT_')}
with (root/'app.log').open('wb') as log:
    child=subprocess.Popen([str(app)],cwd=REPO/'client',env=env,stdout=log,stderr=log);stages=set();deadline=time.monotonic()+100
    try:
        while time.monotonic()<deadline:
            output=(root/'app.log').read_text()
            for stage in ('PLACEHOLDER','POSTER','PAUSED','ENDED','TOP'):
                if 'video-poster-smoke: '+stage in output and stage not in stages:
                    stages.add(stage);time.sleep(.1);subprocess.run(['/usr/bin/import','-window','root',str(root/(stage.lower()+'.png'))],check=True,env=env)
            if child.poll() is not None:break
            time.sleep(.03)
        else:raise RuntimeError('private video poster walk timeout')
        print('\n'.join(line for line in output.splitlines() if 'video-poster-smoke:' in line)); assert child.returncode==0,output
        calls=[json.loads(line) for line in (root/'calls.jsonl').read_text().splitlines()]
        assert len([c for c in calls if c and c[0]=='attachment'])==1,calls
        assert not any('download' in c or 'start' in c or 'send' in c or 'session-action' in c for c in calls)
        print('PASS private source JPEG via zerus-file; offscreen/once/cancellation; fixed geometry; real private MP4 play/pause/end; no remote probes or other effects')
    finally:
        if child.poll() is None:child.terminate();child.wait(timeout=5)
