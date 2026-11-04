#!/usr/bin/env python3
"""Qualify the built Linux Native UI against local HTTP and fake hgs only.

The outer process always starts a fresh private D-Bus, Xvfb and allowlisted
HOME/XDG/TMPDIR/TMUX_TMPDIR. Build the app with the switch identity first:
TAURI_CONFIG='{"identifier":"com.hgdev.hgs-tray"}' cargo build --locked     --manifest-path client/src-tauri/Cargo.toml --features media,tauri/custom-protocol
Then run this script with --app and a new --output directory.
"""
import os, pathlib, subprocess, tempfile, json, importlib.util, shutil, re, time, threading, http.server, urllib.parse, unittest
ROOT = pathlib.Path(__file__).resolve().parents[2]
CLIENT = ROOT / 'client'
BINARY = pathlib.Path(os.environ.get('ZERUS_NATIVE_SMOKE_APP', str(CLIENT / 'src-tauri/target/debug/zerus-client')))
OUTPUT = pathlib.Path(os.environ.get('ZERUS_NATIVE_SMOKE_OUTPUT', '/nonexistent-native-smoke-output'))
PAGE = r'''<!doctype html><meta charset="utf-8"><title>Private native fixture</title><style>html,body{margin:0;background:#1a604e;color:white;font:16px sans-serif;height:100%}h1{margin:20px}a{display:block;margin:20px;color:white}</style><h1>Private DeepSeek workspace</h1><a id="same" href="/second">Same-origin page</a><a id="external" href="https://example.invalid/native-docs">External link</a><a id="popup" target="_blank" href="/popup">Same-origin popup</a><a id="download" href="/download">Download binary fixture</a><a id="download-text" href="/download-text">Download text attachment</a><a id="download-html" href="/download-html">Download HTML attachment</a><a id="blank" target="_blank" href="https://example.invalid/native-blank">External new window link</a><button id="window-open" onclick="window.open('https://example.invalid/window-open')">User window.open</button><a id="redirect" href="/redirect">External redirect (deny)</a><button id="script" onclick="location.href='https://example.invalid/script'">Script during a real gesture (deny)</button><script>
const bounded = p => Promise.race([p, new Promise((_,reject)=>setTimeout(()=>reject(Error('TIMEOUT')),3000))]);
const checks=[];const scriptPopupBlocked=window.open('https://example.invalid/unprompted')===null;
for(const type of ['pointerdown','click']) document.addEventListener(type,e=>fetch('/interaction',{method:'POST',body:JSON.stringify({type,id:e.target.id,trusted:e.isTrusted,x:e.clientX,y:e.clientY,width:innerWidth,height:innerHeight})}));
async function command(name) {
 if (!window.__TAURI_INTERNALS__?.invoke) { checks.push({name,blocked:true,reason:'not exposed'});return; }
 try { await bounded(window.__TAURI_INTERNALS__.invoke(name)); checks.push({name,blocked:false}); }
 catch(e) { const reason=String(e);checks.push({name,blocked:/not allowed|not permitted|denied|forbidden/i.test(reason),reason}); }
}
async function resource(url) {
 try { const r=await bounded(fetch(url));checks.push({name:url,blocked:!r.ok,status:r.status}); }
 catch(e) { checks.push({name:url,blocked:!String(e).includes('TIMEOUT'),reason:String(e)}); }
}
(async()=>{
 await Promise.all(['build_info','metrics','plugin:webview|get_all_webviews','plugin:opener|open_url'].map(command));
 await Promise.all(['tauri://localhost/index.html','asset://localhost/private-fixture','ipc://localhost/probe','zerus-file://localhost/private-fixture','file:///private-fixture'].map(resource));
 await fetch('/report',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({path:location.pathname,checks,scriptPopupBlocked,rects:Object.fromEntries([...document.querySelectorAll('a,button')].map(el=>{const r=el.getBoundingClientRect();return [el.id,{x:r.x,y:r.y,width:r.width,height:r.height}]}))})});
})();</script>'''

class NativeSmoke(unittest.TestCase):

    def test_private_native_child_and_window_paths(self):
        folder = self.enterContext(tempfile.TemporaryDirectory(prefix='zerus-native-ui-app-'))
        base = pathlib.Path(folder)
        reports = []
        requests = []

        class Server(http.server.BaseHTTPRequestHandler):

            def log_message(self, *_):
                pass

            def do_GET(self):
                path = urllib.parse.urlparse(self.path).path
                requests.append(path)
                if path == '/redirect':
                    self.send_response(302)
                    self.send_header('Location', 'https://example.invalid/redirect')
                    self.end_headers()
                    return
                if path in ('/download', '/download-text', '/download-html'):
                    body = b'<script>fetch("/unexpected-html-execution")</script><p>download only</p>' if path == '/download-html' else b'private synthetic download'+bytes([10])
                    self.send_response(200)
                    self.send_header('Content-Disposition', 'attachment; filename="fixture.txt"')
                    self.send_header('Content-Type', {'/download-text': 'text/plain', '/download-html': 'text/html'}.get(path, 'application/octet-stream'))
                else:
                    body = PAGE.encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/html')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                length = int(self.headers.get('Content-Length', '0'))
                assert length < 65536
                payload = json.loads(self.rfile.read(length))
                if self.path == '/interaction':
                    print('INTERACTION', payload, flush=True)
                else:
                    reports.append(payload)
                self.send_response(204)
                self.end_headers()
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        env = {k: os.environ[k] for k in ['PATH', 'LANG', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY', 'GDK_BACKEND', 'WAYLAND_DISPLAY', 'NO_AT_BRIDGE', 'WEBKIT_DISABLE_DMABUF_RENDERER'] if k in os.environ}
        for key, sub in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'), ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'), ('XDG_STATE_HOME', 'xdg-state'), ('XDG_RUNTIME_DIR', 'runtime'), ('HGS_STATE_DIR', 'state'), ('TMPDIR', 'tmp'), ('TMUX_TMPDIR', 'tmux')]:
            p = base / sub
            p.mkdir(mode=0o700)
            env[key] = str(p)
        spec = importlib.util.spec_from_file_location('phase4', CLIENT / 'scripts/phase4-smoke.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        config = module.fixture_config()
        config['native_ui'] = {'deepseek/docs-site/examples': f'http://127.0.0.1:{server.server_port}/?token=fixture'}
        (base / 'config.json').write_text(json.dumps(config))
        hgs = base / 'hgs'
        shutil.copy2(CLIENT / 'src-tauri/tests/support/fake_hgs_phase4.py', hgs)
        hgs.chmod(0o700)
        file = base / 'smoke-notes.txt'
        file.write_text('{"orders":[1,2,3]}\n')
        helpers = base / 'helpers'
        helpers.mkdir()
        for name in ['hgs', 'systemctl', 'launchctl']:
            p = helpers / name
            p.write_text('#!/bin/sh\nexit 1\n')
            p.chmod(0o700)
        for name in ['xdg-open', 'gio', 'sensible-browser']:
            p = helpers / name
            p.write_text('#!/usr/bin/python3\nimport pathlib,json,sys\np=pathlib.Path(__file__).parent/"opener.ndjson"\nwith p.open("a") as f:f.write(json.dumps(sys.argv)+"\\n")\n')
            p.chmod(0o700)
        env['GSETTINGS_BACKEND'] = 'memory'
        env['PYTHONWARNINGS'] = 'ignore::DeprecationWarning'
        env['PATH'] = str(helpers) + os.pathsep + env['PATH']
        env.update(ZERUS_CLIENT_HEADLESS='1', ZERUS_CLIENT_REPLAY=str(CLIENT / 'fixtures/replay.ndjson'), ZERUS_CLIENT_HGS=str(hgs), ZERUS_CLIENT_QUERY='native-ui-smoke=1', ZERUS_CLIENT_PERF='smoke', ZERUS_CLIENT_SMOKE_FILE=str(file), ZERUS_CLIENT_SMOKE_NOW='1793000100')
        for k in ['HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'TMPDIR', 'TMUX_TMPDIR']:
            os.environ[k] = env[k]
        import gi
        gi.require_version('Gtk', '3.0')
        gi.require_version('Gdk', '3.0')
        from gi.repository import Gtk, Gdk
        Gtk.init([])
        info = subprocess.run([str(BINARY), '--build-info'], cwd=base, env=env, capture_output=True, text=True, timeout=8)
        self.assertEqual(info.returncode, 0, info.stderr)
        self.assertEqual(json.loads(info.stdout)['CFBundleIdentifier'], 'com.hgdev.hgs-tray', 'build the switch identity as documented above')
        log = OUTPUT / 'app.log'
        def save_evidence():
            (OUTPUT / 'http-results.json').write_text(json.dumps({'reports': reports, 'requests': requests}, indent=2))
            if (base / 'log.ndjson').exists():
                shutil.copy2(base / 'log.ndjson', OUTPUT / 'calls.ndjson')
        self.addCleanup(save_evidence)
        stages = set()
        rectangle = None

        def screen(stage):
            root = Gdk.get_default_root_window()
            pix = Gdk.pixbuf_get_from_window(root, 0, 0, 1440, 900)
            pix.savev(str(OUTPUT / (stage + '.png')), 'png', [], [])
            x = int(rectangle['x'] + rectangle['width'] * 0.8)
            y = int(rectangle['y'] + rectangle['height'] * 0.8)
            offset = y * pix.get_rowstride() + x * pix.get_n_channels()
            return tuple(pix.get_pixels()[offset:offset + 3])

        def advance(stage):
            with (base / 'log.ndjson').open('a') as f:
                f.write(json.dumps({'argv': ['native-smoke-control', stage], 'stdin': ''}) + '\n')
        command = [str(BINARY), '--sessions']
        print('EXECUTED', json.dumps(command), 'private bus/Xvfb/allowlist HOME/XDG/TMPDIR/TMUX_TMPDIR', flush=True)
        with log.open('w') as output:
            process = subprocess.Popen(command, cwd=base, env=env, stdout=output, stderr=subprocess.STDOUT)

            def cleanup():
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=8)
            self.addCleanup(cleanup)
            deadline = time.monotonic() + 180
            while process.poll() is None and time.monotonic() < deadline:
                raw = log.read_text()
                for marker, key, visible in [('READY', 'ready', True), ('MENU', 'menu', False), ('DIALOG', 'dialog', False), ('RESTORED', 'restored', True), ('RELOADED', 'reloaded', True)] + [(f'PROFILE_CHANGE {i}', f'profile-change-{i}', None) for i in range(1, 6)] + [(f'PROFILE_READY {i}', f'profile-ready-{i}', True) for i in range(1, 6)] + [('FALLBACK', 'fallback', False), ('FINAL_READY', 'final-ready', True)]:
                    if key in stages or 'native-ui-smoke: ' + marker not in raw:
                        continue
                    if marker.startswith('PROFILE_CHANGE'):
                        index = int(marker.split()[-1])
                        config['native_ui']['deepseek/docs-site/examples'] = f'http://127.0.0.1:{server.server_port}/?token=fixture{index}'
                        (base / 'config.json').write_text(json.dumps(config))
                        advance(key)
                        stages.add(key)
                        continue
                    if marker == 'READY':
                        m = re.search('native-ui-smoke: READY (\\{[^\\n]+\\})', raw)
                        self.assertIsNotNone(m, raw)
                        rectangle = json.loads(m.group(1))
                        if not reports:
                            continue
                        self.assertTrue(all((c['blocked'] for c in reports[0]['checks'])), reports)
                        self.assertEqual(len(reports[0]['checks']), 9, reports)
                        self.assertTrue(reports[0]['scriptPopupBlocked'], reports)
                        print('PASS actual child IPC and private protocols refused', json.dumps(reports[0]), flush=True)
                        import ctypes
                        xlib = ctypes.CDLL('libX11.so.6')
                        xtst = ctypes.CDLL('libXtst.so.6')
                        xlib.XOpenDisplay.restype = ctypes.c_void_p
                        display = xlib.XOpenDisplay(None)
                        self.assertTrue(display)
                        xlib.XFlush.argtypes = [ctypes.c_void_p]
                        xlib.XCloseDisplay.argtypes = [ctypes.c_void_p]
                        self.addCleanup(lambda: xlib.XCloseDisplay(display))
                        xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
                        xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]

                        def click(ident):
                            r = reports[-1]['rects'][ident]
                            x = int(rectangle['x'] + r['x'] + min(r['width'] / 2, 60))
                            y = int(rectangle['y'] + r['y'] + r['height'] / 2)
                            xtst.XTestFakeMotionEvent(display, -1, x, y, 0)
                            xtst.XTestFakeButtonEvent(display, 1, 1, 0)
                            xtst.XTestFakeButtonEvent(display, 1, 0, 0)
                            xlib.XFlush(display)

                        def wait_for(test, why):
                            end = time.monotonic() + 4
                            while not test():
                                while Gtk.events_pending():
                                    Gtk.main_iteration_do(False)
                                if time.monotonic() >= end:
                                    screen('failure')
                                    print(subprocess.check_output(['/usr/bin/xwininfo', '-root', '-tree'], env=env, text=True), flush=True)
                                self.assertLess(time.monotonic(), end, why + '\n' + log.read_text()[-2500:])
                                time.sleep(0.02)

                        def opened():
                            f = helpers / 'opener.ndjson'
                            return [json.loads(line) for line in f.read_text().splitlines()] if f.exists() else []
                        time.sleep(0.5)
                        screen('before-click')
                        click('external')
                        wait_for(lambda: len(opened()) == 1, 'real external link did not reach guarded opener')
                        self.assertIn('https://example.invalid/native-docs', opened()[0])
                        time.sleep(0.1)
                        click('script')
                        time.sleep(0.25)
                        self.assertEqual(len(opened()), 1, 'script navigation obtained user-link authority')
                        click('redirect')
                        wait_for(lambda: '/redirect' in requests, 'same-origin redirect was not requested')
                        time.sleep(0.25)
                        self.assertEqual(len(opened()), 1, 'redirect obtained user-link authority')
                        previous = len(reports)
                        click('same')
                        wait_for(lambda: len(reports) > previous and reports[-1]['path'] == '/second', 'same-origin link did not navigate')
                        self.assertEqual(len(opened()), 1)
                        previous = len(reports)
                        click('popup')
                        wait_for(lambda: len(reports) > previous and reports[-1]['path'] == '/popup', 'same-origin popup did not stay inside original child')
                        self.assertEqual(len(opened()), 1)
                        print('PASS actual XTest gestures: one external link; script and redirect refused; same-origin page and popup retained', flush=True)
                        previous = len(opened())
                        click('blank')
                        wait_for(lambda: len(opened()) == previous + 1, 'target=_blank link did not use guarded opener')
                        self.assertIn('https://example.invalid/native-blank', opened()[-1])
                        previous = len(opened())
                        click('window-open')
                        wait_for(lambda: len(opened()) == previous + 1, 'user window.open did not use guarded opener')
                        self.assertIn('https://example.invalid/window-open', opened()[-1])
                        print('PASS target=_blank and user-initiated window.open create no native window and use guarded browser handoff', flush=True)
                        xlib.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
                        xlib.XStringToKeysym.argtypes = [ctypes.c_char_p]
                        xlib.XStringToKeysym.restype = ctypes.c_ulong
                        xlib.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
                        xlib.XKeysymToKeycode.restype = ctypes.c_uint
                        xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]

                        def key_event(symbol, down=True):
                            xtst.XTestFakeKeyEvent(display, xlib.XKeysymToKeycode(display, xlib.XStringToKeysym(symbol.encode())), down, 0)
                            xlib.XFlush(display)

                        def press(symbol):
                            key_event(symbol)
                            key_event(symbol, False)

                        def windows(title):
                            tree = subprocess.check_output(['/usr/bin/xwininfo', '-root', '-tree'], env=env, text=True)
                            return [int(line.split()[0], 16) for line in tree.splitlines() if title in line and line.strip().startswith('0x')]

                        def save_dialog():
                            result = windows('"Save file"')
                            return result[0] if result else None

                        def focus(w):
                            xlib.XSetInputFocus(display, w, 1, 0)
                            xlib.XFlush(display)
                        click('download')
                        wait_for(save_dialog, 'download did not open guarded Save')
                        focus(save_dialog())
                        screen('download-cancel')
                        press('Escape')
                        wait_for(lambda: not save_dialog(), 'download cancel left a dialog')
                        self.assertFalse(list(base.rglob('fixture.txt')), 'cancelled download wrote a default file')
                        click('download-text')
                        wait_for(save_dialog, 'text attachment did not open guarded Save')
                        focus(save_dialog())
                        key_event('Control_L')
                        press('l')
                        key_event('Control_L', False)
                        time.sleep(0.12)
                        destination = base / 'saved-fixture.txt'
                        clipboard = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
                        clipboard.set_text(str(destination), -1)
                        key_event('Control_L')
                        press('a')
                        press('v')
                        key_event('Control_L', False)
                        paste_end = time.monotonic() + 0.3
                        while time.monotonic() < paste_end:
                            while Gtk.events_pending():
                                Gtk.main_iteration_do(False)
                            time.sleep(0.01)
                        press('Return')
                        time.sleep(0.15)
                        press('Return')
                        wait_for(lambda: destination.exists(), 'explicit guarded download did not write its exact private destination')
                        self.assertEqual(destination.read_bytes(), b'private synthetic download' + bytes([10]))
                        self.assertFalse(save_dialog())
                        print('PASS guarded download: Cancel writes nothing; explicit Save writes exact bytes in private scratch', flush=True)
                        click('download-html')
                        wait_for(save_dialog, 'HTML attachment rendered instead of asking for Save')
                        self.assertNotIn('/unexpected-html-execution', requests, 'HTML attachment executed in the child')
                        focus(save_dialog()); press('Escape')
                        wait_for(lambda: not save_dialog(), 'HTML attachment cancel left a dialog')
                        self.assertFalse(list(base.rglob('fixture.txt')), 'cancelled HTML attachment wrote a file')
                        self.assertNotIn('/unexpected-html-execution', requests, 'cancelled HTML attachment executed in the child')
                        print('PASS attachment disposition before MIME: binary Cancel, text Save exact bytes, HTML Cancel without rendering or script execution', flush=True)

                        for flags in [[], ['--sessions']]:
                            second = subprocess.run([str(BINARY), *flags], env=env, capture_output=True, text=True, timeout=8)
                            self.assertEqual(second.returncode, 0, second.stderr)
                            self.assertIn('already running', second.stderr)
                        primary = subprocess.run(['/usr/bin/gdbus', 'call', '--session', '--dest', f'org.kde.StatusNotifierItem-{process.pid}-1', '--object-path', '/StatusNotifierItem', '--method', 'org.kde.StatusNotifierItem.Activate', '0', '0'], env=env, capture_output=True, text=True, timeout=8)
                        self.assertEqual(primary.returncode, 0, primary.stderr)
                    if marker == 'RELOADED':
                        previous = len(opened())
                        click('external')
                        wait_for(lambda: len(opened()) == previous + 1, 'reload lost guarded external links')
                        previous = len(opened())
                        click('script')
                        time.sleep(0.15)
                        self.assertEqual(len(opened()), previous, 'reload lost the script navigation guard')
                        main = windows('"Zerus Next"')[0]
                        xlib.XResizeWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_uint, ctypes.c_uint]
                        old_width = rectangle['width']
                        xlib.XResizeWindow(display, main, 1200, 800)
                        xlib.XFlush(display)

                        def resized():
                            found = re.findall('native-ui-smoke: GEOMETRY (\\{[^\\n]+\\})', log.read_text())
                            return json.loads(found[-1]) if found and json.loads(found[-1])['width'] < old_width else None
                        wait_for(resized, 'main resize did not remeasure the native frame')
                        rectangle = resized()
                        time.sleep(0.25)
                        self.assertEqual(screen('resized'), (26, 96, 78))
                        from PIL import Image
                        resized_image = Image.open(str(OUTPUT / 'resized.png')).convert('RGB')
                        self.assertNotEqual(resized_image.getpixel((int(rectangle['x'] + rectangle['width'] + 5), int(rectangle['y'] + 50))), (26, 96, 78), 'resized child covered inspector')
                        xlib.XResizeWindow(display, main, 1440, 900)
                        xlib.XFlush(display)
                        wait_for(lambda: (found := re.findall('native-ui-smoke: GEOMETRY (\\{[^\\n]+\\})', log.read_text())) and json.loads(found[-1])['width'] == old_width, 'main resize did not restore frame')
                        rectangle = json.loads(re.findall('native-ui-smoke: GEOMETRY (\\{[^\\n]+\\})', log.read_text())[-1])
                        time.sleep(0.2)

                        class MessageData(ctypes.Union):
                            _fields_ = [('b', ctypes.c_char * 20), ('s', ctypes.c_short * 10), ('l', ctypes.c_long * 5)]

                        class Message(ctypes.Structure):
                            _fields_ = [('type', ctypes.c_int), ('serial', ctypes.c_ulong), ('send_event', ctypes.c_int), ('display', ctypes.c_void_p), ('window', ctypes.c_ulong), ('message_type', ctypes.c_ulong), ('format', ctypes.c_int), ('data', MessageData)]

                        class Event(ctypes.Union):
                            _fields_ = [('message', Message), ('pad', ctypes.c_long * 24)]
                        xlib.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
                        xlib.XInternAtom.restype = ctypes.c_ulong
                        xlib.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.POINTER(Event)]

                        def mapped():
                            return 'Map State: IsViewable' in subprocess.check_output(['/usr/bin/xwininfo', '-id', hex(main)], env=env, text=True)

                        def close_window():
                            event = Event()
                            event.message.type = 33
                            event.message.display = display
                            event.message.window = main
                            event.message.message_type = xlib.XInternAtom(display, b'WM_PROTOCOLS', False)
                            event.message.format = 32
                            event.message.data.l[0] = xlib.XInternAtom(display, b'WM_DELETE_WINDOW', False)
                            self.assertTrue(xlib.XSendEvent(display, main, False, 0, ctypes.byref(event)))
                            xlib.XFlush(display)
                            wait_for(lambda: not mapped(), 'Close did not hide main with native child')
                        close_window()
                        for flags in [[], ['--background']]:
                            second = subprocess.run([str(BINARY), *flags], cwd=base, env=env, capture_output=True, text=True, timeout=8)
                            self.assertEqual(second.returncode, 0, second.stderr)
                            self.assertFalse(mapped(), 'plain Show presented hidden window')
                        second = subprocess.run([str(BINARY), '--sessions'], cwd=base, env=env, capture_output=True, text=True, timeout=8)
                        self.assertEqual(second.returncode, 0, second.stderr)
                        wait_for(mapped, '--sessions did not present retained main with native child')
                        wait_for(lambda: screen('sessions-restored') == (26, 96, 78), '--sessions did not restore native frame after showing main')
                        close_window()
                        primary = subprocess.run(['/usr/bin/gdbus', 'call', '--session', '--dest', f'org.kde.StatusNotifierItem-{process.pid}-1', '--object-path', '/StatusNotifierItem', '--method', 'org.kde.StatusNotifierItem.Activate', '0', '0'], env=env, capture_output=True, text=True, timeout=8)
                        self.assertEqual(primary.returncode, 0, primary.stderr)
                        wait_for(mapped, 'SNI primary did not present retained main')
                        wait_for(lambda: screen('tray-restored') == (26, 96, 78), 'SNI did not restore native frame after showing main')
                        self.assertIsNone(process.poll())
                        print('PASS reload guards, resize clipping, Close/Show/background/sessions/SNI with same native child and PID', flush=True)
                    time.sleep(0.12)
                    pixel = screen(key)
                    if visible:
                        from PIL import Image
                        image = Image.open(str(OUTPUT / (key + '.png'))).convert('RGB')
                        for point in [(30, 30), (300, 400), (1100, 400), (500, 80), (390, 120), (int(rectangle['x'] - 3), 400)]:
                            self.assertNotEqual(image.getpixel(point), (26, 96, 78), (marker, 'native child escaped frame', point))
                    self.assertEqual(pixel == (26, 96, 78), visible, (marker, pixel, raw[-2500:]))
                    print('PASS native child placement', marker, 'pixel', pixel, flush=True)
                    advance(key)
                    stages.add(key)
                time.sleep(0.02)
            if process.poll() is None:
                cleanup()
                self.fail('native smoke timed out: ' + log.read_text()[-6000:])
        raw = log.read_text()
        print(raw, flush=True)
        self.assertEqual(process.returncode, 0, raw[-8000:])
        self.assertEqual(stages, {'ready', 'menu', 'dialog', 'restored', 'reloaded', 'fallback', 'final-ready'} | {f'profile-change-{i}' for i in range(1, 6)} | {f'profile-ready-{i}' for i in range(1, 6)})
        self.assertIn('native-ui-smoke: PASS', raw)
        steps = re.findall('phase4-smoke ([^:]+): done', raw)
        self.assertGreaterEqual(len(steps), 16)
        print('PASS full Phase4 with actual native child retained:', len(steps), steps, flush=True)
        self.assertFalse((pathlib.Path(env['TMPDIR']) / 'hgs-tray').exists(), 'canonical socket survived normal exit')

def main():
    import argparse
    import sys
    import uuid
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=pathlib.Path, default=CLIENT / 'src-tauri/target/debug/zerus-client')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--private-run', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.private_run:
        if args.private_run != os.environ.get('ZERUS_NATIVE_SMOKE_PRIVATE'):
            parser.error('the private inner process must be launched by this script')
        unittest.main(argv=[sys.argv[0]], verbosity=2)
        return 0
    output = args.output.resolve()
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    app = args.app.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='zerus-native-ui-private-') as folder:
        base = pathlib.Path(folder)
        env = {'PATH': '/usr/bin:/bin:/usr/local/bin', 'LANG': 'C.UTF-8'}
        for key, name in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'), ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'), ('XDG_STATE_HOME', 'xdg-state'), ('XDG_RUNTIME_DIR', 'runtime'), ('HGS_STATE_DIR', 'state'), ('TMPDIR', 'tmp'), ('TMUX_TMPDIR', 'tmux')]:
            path = base / name
            path.mkdir(mode=0o700)
            env[key] = str(path)
        nonce = str(uuid.uuid4())
        env.update(GDK_BACKEND='x11', WAYLAND_DISPLAY='zerus-native-ui-nonexistent-wayland', NO_AT_BRIDGE='1', WEBKIT_DISABLE_DMABUF_RENDERER='1', GSETTINGS_BACKEND='memory', PYTHONWARNINGS='ignore::DeprecationWarning', ZERUS_NATIVE_SMOKE_PRIVATE=nonce, ZERUS_NATIVE_SMOKE_APP=str(app), ZERUS_NATIVE_SMOKE_OUTPUT=str(output))
        config = base / 'bus.conf'
        config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=' + str(base) + '</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow own="*"/><allow receive_sender="*"/><allow eavesdrop="true"/></policy></busconfig>')
        command = ['dbus-run-session', '--config-file', str(config), '--', 'xvfb-run', '-a', '-s', '-screen 0 1440x900x24', sys.executable, str(pathlib.Path(__file__).resolve()), '--app', str(app), '--output', str(output), '--private-run', nonce]
        print('EXECUTED private D-Bus/Xvfb; child env from scratch; no owner desktop/session bus', flush=True)
        process = subprocess.Popen(command, cwd=ROOT, env=env, start_new_session=True)
        try:
            return process.wait(timeout=240)
        except subprocess.TimeoutExpired:
            import signal
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=8)
            return 124
if __name__ == '__main__':
    raise SystemExit(main())
