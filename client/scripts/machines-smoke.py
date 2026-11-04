#!/usr/bin/env python3
"""Run the real Machines editor with private fake profiles, D-Bus and Xvfb."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

CLIENT = Path(__file__).resolve().parent.parent

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--inner', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--tools', action='store_true', help='Exercise the configured SSH terminal and native key picker')
    parser.add_argument('--setup', action='store_true', help='Exercise confirmed setup with the private fake CLI only')
    args = parser.parse_args()
    assert not (args.setup and args.tools)
    app, output = args.app.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if not args.inner:
        private = Path(tempfile.mkdtemp(prefix='zerus-machines-smoke-'))
        private.chmod(0o700)
        env = {'PATH':'/usr/bin:/bin', 'LANG':'C.UTF-8', 'GDK_BACKEND':'x11', 'WAYLAND_DISPLAY':'zerus-nonexistent',
               'NO_AT_BRIDGE':'1', 'WEBKIT_DISABLE_DMABUF_RENDERER':'1', 'GSETTINGS_BACKEND':'memory',
               'ZERUS_MACHINES_PRIVATE_ROOT':str(private)}
        for key, folder in [('HOME','home'),('XDG_CONFIG_HOME','config'),('XDG_DATA_HOME','data'),('XDG_CACHE_HOME','cache'),
                            ('XDG_STATE_HOME','state'),('XDG_RUNTIME_DIR','runtime'),('TMPDIR','tmp'),('TMUX_TMPDIR','tmux'),
                            ('HGS_STATE_DIR','hgs-state'),('HGS_CONFIG_DIR','hgs-config')]:
            path = private / folder;path.mkdir(mode=0o700);env[key] = str(path)
        with (output/'driver.log').open('w') as log:
            command = ['dbus-run-session','--','xvfb-run','-a','-s','-screen 0 1440x900x24',sys.executable,__file__,
                       '--app',str(app),'--output',str(output),'--inner']
            if args.tools:command.append('--tools')
            if args.setup:command.append('--setup')
            log.write('EXECUTED '+json.dumps(command)+'\nFresh allowlisted private HOME/XDG/TMPDIR/TMUX_TMPDIR/D-Bus/Xvfb\n');log.flush()
            result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=100)
            log.write('\nEXIT_CODE '+str(result.returncode)+'\n')
        # The driver already retained bounded call/profile evidence. Do not
        # copy browser caches, runtime sockets or an entire scratch tree.
        shutil.rmtree(private)
        print((output/'driver.log').read_text()[-2500:])
        return result.returncode

    private = Path(os.environ.get('ZERUS_MACHINES_PRIVATE_ROOT','/nonexistent-private-machines-root'))
    assert private.name.startswith('zerus-machines-smoke-') and private.is_dir(),'run the outer private launcher'
    for key in ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','XDG_RUNTIME_DIR','TMPDIR','TMUX_TMPDIR','HGS_STATE_DIR','HGS_CONFIG_DIR']:
        assert Path(os.environ[key]).parent == private, key
    assert os.environ.get('DISPLAY') and os.environ.get('DBUS_SESSION_BUS_ADDRESS')
    scratch = Path(os.environ['TMPDIR'])/'machines-fixture';scratch.mkdir(mode=0o700)
    fake = scratch/'hgs';shutil.copy2(CLIENT/'src-tauri/tests/support/fake_hgs_machines.py',fake);fake.chmod(0o700)
    (scratch/'config.json').write_text(json.dumps({'machines_fixture':True,'disabled_peer':args.tools,'setup_fixture':args.setup}))
    helpers = scratch/'helpers';helpers.mkdir(mode=0o700)
    for name in ['hgs','systemctl','launchctl','xdg-open','gio','sensible-browser']:
        file=helpers/name;file.write_text('#!/bin/sh\nexit 1\n');file.chmod(0o700)
    if args.tools:
        terminal=helpers/'private-terminal'
        terminal.write_text('#!/usr/bin/python3\nfrom pathlib import Path\nimport json,sys\nwith (Path(__file__).parent.parent/"log.ndjson").open("a") as f:f.write(json.dumps({"argv":["private-terminal",*sys.argv[1:]],"stdin":""})+"\\n")\n')
        terminal.chmod(0o700)
        conf=Path(os.environ['HOME'])/'.config/hgs';conf.mkdir(mode=0o700,parents=True)
        (conf/'tray.conf').write_text('terminal='+str(terminal)+' --command {cmd}\n')
    env={key:os.environ[key] for key in ['PATH','LANG','DISPLAY','DBUS_SESSION_BUS_ADDRESS','XAUTHORITY','HOME','XDG_CONFIG_HOME',
        'XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','XDG_RUNTIME_DIR','TMPDIR','TMUX_TMPDIR','HGS_STATE_DIR','HGS_CONFIG_DIR',
        'GDK_BACKEND','WAYLAND_DISPLAY','NO_AT_BRIDGE','WEBKIT_DISABLE_DMABUF_RENDERER','GSETTINGS_BACKEND'] if key in os.environ}
    env.update(PATH=str(helpers)+':/usr/bin:/bin',ZERUS_CLIENT_HEADLESS='1',ZERUS_CLIENT_REPLAY=str(CLIENT/'fixtures/replay.ndjson'),
               ZERUS_CLIENT_HGS=str(fake),ZERUS_CLIENT_QUERY='machines-tools-smoke=1' if args.tools else 'machines-smoke=1',ZERUS_CLIENT_PERF='smoke')
    if args.setup:
        frames = [json.loads(line) for line in (CLIENT/'fixtures/replay.ndjson').read_text().splitlines()]
        for frame in frames:
            if frame.get('topic') == 'catalog.meta':
                frame['value'].update(local_sessions=False,local_host={'state':'not-installed','protocol':1})
                frame['value']['metrics']['platform']='windows'
        replay=scratch/'setup-replay.ndjson';replay.write_text('\n'.join(json.dumps(f) for f in frames)+'\n')
        env.update(ZERUS_CLIENT_REPLAY=str(replay),ZERUS_CLIENT_QUERY='machines-setup-smoke=1')
    import gi
    gi.require_version('Gtk','3.0');gi.require_version('Gdk','3.0')
    from gi.repository import Gtk,Gdk,GLib
    Gtk.init([])
    def pump(seconds):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            while GLib.MainContext.default().pending():GLib.MainContext.default().iteration(False)
            time.sleep(.005)
    if args.tools:
        import ctypes as c
        import ctypes.util
        x=c.CDLL(ctypes.util.find_library('X11'));xt=c.CDLL(ctypes.util.find_library('Xtst'))
        x.XOpenDisplay.argtypes=[c.c_char_p];x.XOpenDisplay.restype=c.c_void_p
        display=x.XOpenDisplay(None);assert display
        x.XDefaultRootWindow.argtypes=[c.c_void_p];x.XDefaultRootWindow.restype=c.c_ulong
        x.XQueryTree.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_ulong),c.POINTER(c.c_ulong),c.POINTER(c.POINTER(c.c_ulong)),c.POINTER(c.c_uint)]
        x.XFetchName.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_char_p)]
        x.XFree.argtypes=[c.c_void_p]
        x.XGetTransientForHint.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_ulong)]
        x.XSetInputFocus.argtypes=[c.c_void_p,c.c_ulong,c.c_int,c.c_ulong]
        x.XStringToKeysym.argtypes=[c.c_char_p];x.XStringToKeysym.restype=c.c_ulong
        x.XKeysymToKeycode.argtypes=[c.c_void_p,c.c_ulong];x.XKeysymToKeycode.restype=c.c_ubyte
        xt.XTestFakeKeyEvent.argtypes=[c.c_void_p,c.c_uint,c.c_int,c.c_ulong]
        x.XFlush.argtypes=[c.c_void_p]
        root_window=x.XDefaultRootWindow(display)
        def name(window):
            value=c.c_char_p()
            if not x.XFetchName(display,window,c.byref(value)) or not value:return ''
            text=value.value.decode(errors='replace');x.XFree(value);return text
        def chooser():
            root=c.c_ulong();parent=c.c_ulong();children=c.POINTER(c.c_ulong)();count=c.c_uint()
            if not x.XQueryTree(display,root_window,c.byref(root),c.byref(parent),c.byref(children),c.byref(count)):return None
            windows=list(children[:count.value]) if children else []
            if children:x.XFree(children)
            return next((window for window in windows if name(window)=='Choose SSH private key'),None)
        def key(symbol,pressed):
            code=x.XKeysymToKeycode(display,x.XStringToKeysym(symbol.encode()));assert code
            xt.XTestFakeKeyEvent(display,code,pressed,0);x.XFlush(display)
        def press(symbol):key(symbol,1);key(symbol,0);pump(.08)
        def chord(symbol):key('Control_L',1);press(symbol);key('Control_L',0);pump(.08)
        key_path=Path(env['HOME'])/'.ssh/id_fixture';key_path.parent.mkdir(mode=0o700)
        key_path.write_text('synthetic path fixture, not a key\n');key_path.chmod(0o600)
        parents=[]
        def answer_picker(select,stage):
            deadline=time.monotonic()+5;window=None
            while not window and time.monotonic()<deadline:window=chooser();pump(.02)
            assert window,'native key chooser window missing'
            # rfd's GTK3 backend currently passes a null parent to
            # gtk_file_chooser_native_new despite the plugin's set_parent.
            # Verify the actual chooser belongs to this private app; retain
            # the parent observation rather than claim unsupported modality.
            pid=subprocess.check_output(['xprop','-id',str(window),'_NET_WM_PID'],text=True)
            assert pid.strip().endswith('= '+str(process.pid)),pid
            parent=c.c_ulong();has_parent=bool(x.XGetTransientForHint(display,window,c.byref(parent)))
            if has_parent:
                assert parent.value not in [0,root_window,window] and 'Zerus' in name(parent.value),(parent.value,name(parent.value))
            parents.append({'chooser':window,'app_pid':process.pid,'parent':parent.value if has_parent else None,
                            'parent_title':name(parent.value) if has_parent else None})
            x.XSetInputFocus(display,window,2,0);x.XFlush(display);pump(.1)
            root=Gdk.get_default_root_window()
            pix=Gdk.pixbuf_get_from_window(root,0,0,1440,900);pix.savev(str(output/('app-'+stage.lower()+'.png')),'png',[],[])
            if not select:press('Escape');return
            Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD).set_text(str(key_path),-1)
            chord('l');chord('v');pump(.2);press('Return');pump(.4)
            if chooser():press('Return')
    captured=set()
    with (output/'app.log').open('w') as log:
        process=subprocess.Popen([str(app),'--sessions'],cwd=scratch,env=env,stdout=log,stderr=subprocess.STDOUT)
        try:
            deadline=time.monotonic()+70
            while process.poll() is None and time.monotonic()<deadline:
                text=(output/'app.log').read_text()
                stages=['SETUP','INSTALLED','UNKNOWN','ENABLE'] if args.setup else ['SSH_OPENED','KEY_CANCEL_WAIT','KEY_PICK_WAIT','PICKED'] if args.tools else ['EXISTING','UNSAVED','NEW','REMOVE']
                prefix='machine-setup-smoke: ' if args.setup else 'machine-tools-smoke: ' if args.tools else 'machines-smoke: '
                for stage in stages:
                    if stage not in captured and prefix+stage in text:
                        # The log arrives before WebKit's next compositor frame.
                        # The page holds each stage for 700 ms.
                        time.sleep(.25)
                        root=Gdk.get_default_root_window()
                        pix=Gdk.pixbuf_get_from_window(root,0,0,1440,900);pix.savev(str(output/('app-'+stage.lower()+'.png')),'png',[],[])
                        captured.add(stage);print('CAPTURE',stage,flush=True)
                        if args.tools and stage in ['KEY_CANCEL_WAIT','KEY_PICK_WAIT']:answer_picker(stage=='KEY_PICK_WAIT',stage)
                pump(.05)
            code=process.poll()
            if code is None:raise RuntimeError('Machines smoke exceeded 70 seconds')
            text=(output/'app.log').read_text()
            assert code==0 and ('machine-setup-smoke: PASS 10' if args.setup else 'machine-tools-smoke: PASS 9' if args.tools else 'machines-smoke: PASS 12') in text,text[-5000:]
            assert len(captured)==4,captured
            calls=[json.loads(line) for line in (scratch/'log.ndjson').read_text().splitlines()]
            writes=[c for c in calls if c['argv'][:1]==['machine'] and c['argv'][1] in ['set','reset','remove']]
            assert len(writes)==(0 if args.tools or args.setup else 4),writes
            assert all('--revision' in c['argv'] for c in writes)
            if args.setup:
                setups=[c for c in calls if c['argv'][:2]==['machine','setup']]
                assert len(setups)==2 and all('--revision' in c['argv'] for c in setups)
                assert sum(c['argv']==['session-host','install','--json'] for c in calls)==1
            if args.tools:
                import shlex,re
                terminals=[c for c in calls if c['argv'][0]=='private-terminal'];assert len(terminals)==1
                assert shlex.split(terminals[0]['argv'][-1])==[str(fake),'machine','ssh','fixture-peer']
                selected=json.loads(re.search(r'machine-tools-smoke: PICKED (\{[^\n]+\})',text).group(1))
                assert selected['key']==str(key_path)
                assert key_path.read_text()=='synthetic path fixture, not a key\n'
                (output/'native-parents.json').write_text(json.dumps(parents,indent=2)+'\n')
            shutil.copy2(scratch/'log.ndjson',output/'calls.ndjson')
            (output/'results.json').write_text(json.dumps({'passed':10 if args.setup else 9 if args.tools else 12,'failed':0,'mutations':len(writes),'captures':sorted(captured),'native_checks':4 if args.tools else 0},indent=2)+'\n')
            print('PASS setup10' if args.setup else 'PASS tools9/native4' if args.tools else 'PASS 12; four exact revision-guarded writes; four screenshots',flush=True)
        finally:
            if process.poll() is None:
                process.terminate()
                try:process.wait(timeout=8)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=8)
            for name in ['log.ndjson','machine-profiles.json']:
                if (scratch/name).exists():
                    shutil.copy2(scratch/name,output/name)
    return 0

if __name__=='__main__':raise SystemExit(main())
