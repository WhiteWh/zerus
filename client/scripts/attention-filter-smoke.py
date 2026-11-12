#!/usr/bin/env python3
"""Screenshot the session list's status tabs on child-session fixtures (zerus-sa9qn.18).

The outer process always starts a fresh private D-Bus, Xvfb and allowlisted
HOME/XDG/TMPDIR/TMUX_TMPDIR/HGS_STATE_DIR inside gui_sandbox.sandboxed() (bwrap
with a private /tmp, /run/user, network and IPC), and the inner process refuses
to start the app while a canonical GUI guard is reachable, so the person's GUI
is never reached. The app runs only against the fake hgs CLI and the synthetic
replay; src/perf/attentionFilterSmoke.ts replaces the local rows with child
fixtures, checks that every tab lists exactly what its count counts, and opens
Needs attention for the screenshot. Build the app with the switch identity first:
TAURI_CONFIG='{"identifier":"com.hgdev.hgs-tray"}' cargo build --locked --manifest-path client/src-tauri/Cargo.toml --features media,tauri/custom-protocol
Then run this script with --app and a new --output directory.
"""
import argparse
import importlib.util
import json
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import gui_sandbox  # noqa: E402  (shared GUI isolation, zerus-5a99f)

ROOT = pathlib.Path(__file__).resolve().parents[2]
CLIENT = ROOT / 'client'
PRIVATE = ['HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'HGS_STATE_DIR', 'TMPDIR', 'TMUX_TMPDIR']
NAMES = dict(zip(PRIVATE, ['home', 'config', 'data', 'cache', 'xdg-state', 'runtime', 'state', 'tmp', 'tmux']))


def inner(app: pathlib.Path, output: pathlib.Path) -> int:
    gui_sandbox.refuse_reachable_guard()
    base = pathlib.Path(tempfile.mkdtemp(prefix='zerus-attention-filter-'))
    env = {k: os.environ[k] for k in ['PATH', 'LANG', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY', 'GDK_BACKEND', 'WAYLAND_DISPLAY',
                                      'NO_AT_BRIDGE', 'WEBKIT_DISABLE_DMABUF_RENDERER'] if k in os.environ}
    for key in PRIVATE:
        path = base / NAMES[key]
        path.mkdir(mode=0o700)
        env[key] = str(path)
    spec = importlib.util.spec_from_file_location('phase4', CLIENT / 'scripts/phase4-smoke.py')
    phase4 = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(phase4)
    (base / 'config.json').write_text(json.dumps(phase4.fixture_config()))
    hgs = base / 'hgs'
    shutil.copy2(CLIENT / 'src-tauri/tests/support/fake_hgs_phase4.py', hgs)
    hgs.chmod(0o700)
    helpers = base / 'helpers'
    helpers.mkdir()
    for name in ['hgs', 'systemctl', 'launchctl', 'xdg-open', 'gio']:
        stub = helpers / name
        stub.write_text('#!/bin/sh\nexit 1\n')
        stub.chmod(0o700)
    env.update(PATH=str(helpers) + os.pathsep + env['PATH'], GSETTINGS_BACKEND='memory', ZERUS_CLIENT_HEADLESS='1',
               ZERUS_CLIENT_REPLAY=str(CLIENT / 'fixtures/replay.ndjson'), ZERUS_CLIENT_HGS=str(hgs),
               ZERUS_CLIENT_QUERY='attention-filter-smoke=1', ZERUS_CLIENT_PERF='smoke', ZERUS_CLIENT_SMOKE_NOW='1793000100')
    for key in PRIVATE:
        os.environ[key] = env[key]
    import gi
    gi.require_version('Gdk', '3.0')
    gi.require_version('Gtk', '3.0')
    from gi.repository import Gdk, Gtk
    Gtk.init([])
    log = output / 'app.log'
    command = [str(app), '--sessions']
    print('EXECUTED', json.dumps(command), 'private bus/Xvfb/allowlist HOME/XDG/TMPDIR/TMUX_TMPDIR/HGS_STATE_DIR', flush=True)
    status = 1
    with log.open('w') as sink:
        process = subprocess.Popen(command, cwd=base, env=env, stdout=sink, stderr=subprocess.STDOUT)
        try:
            deadline, shot = time.monotonic() + 150, False
            while process.poll() is None and time.monotonic() < deadline:
                raw = log.read_text()
                if 'attention-filter-smoke: FAIL' in raw:
                    break
                if not shot and 'attention-filter-smoke: READY' in raw:
                    time.sleep(1.0)
                    root = Gdk.get_default_root_window()
                    Gdk.pixbuf_get_from_window(root, 0, 0, 1440, 900).savev(str(output / 'attention-tab.png'), 'png', [], [])
                    shot = True
                if shot and 'attention-filter-smoke: PASS' in raw:
                    status = 0
                    break
                time.sleep(0.25)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=8)
    raw = log.read_text()
    for line in raw.splitlines():
        if 'attention-filter-smoke: ' in line:
            print(line[line.index('attention-filter-smoke: '):], flush=True)
    fixtures = dict(re.findall(r'attention-filter-smoke: FIXTURE (\S+) ([^\n]+)', raw))
    tabs: dict = {}
    for step, tab, body in re.findall(r'attention-filter-smoke: TAB (\w+) (\w+) (\{[^\n]+\})', raw):
        entry = json.loads(body)
        entry['titles'] = [fixtures.get(name, name) for name in entry['listed']]
        tabs.setdefault(step, {})[tab] = entry
    if tabs:
        (output / 'tabs.json').write_text(json.dumps({'fixtures': fixtures, 'tabs': tabs}, indent=2) + '\n')
    shutil.rmtree(base, ignore_errors=True)
    return status


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=pathlib.Path, default=CLIENT / 'src-tauri/target/debug/zerus-client')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--private-run', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if sys.platform != 'linux':
        parser.error('Linux only: other platforms have no private canonical GUI guard here')
    if args.private_run:
        if args.private_run != os.environ.get('ZERUS_ATTENTION_SMOKE_PRIVATE'):
            parser.error('the private inner process must be launched by this script')
        return inner(args.app, args.output)
    output = args.output.resolve()
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    app = args.app.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='zerus-attention-filter-private-') as folder:
        base = pathlib.Path(folder)
        env = {'PATH': '/usr/bin:/bin:/usr/local/bin', 'LANG': 'C.UTF-8'}
        for key in PRIVATE:
            path = base / NAMES[key]
            path.mkdir(mode=0o700)
            env[key] = str(path)
        nonce = str(uuid.uuid4())
        env.update(GDK_BACKEND='x11', WAYLAND_DISPLAY='zerus-attention-nonexistent-wayland', NO_AT_BRIDGE='1', WEBKIT_DISABLE_DMABUF_RENDERER='1',
                   GSETTINGS_BACKEND='memory', PYTHONWARNINGS='ignore::DeprecationWarning', ZERUS_ATTENTION_SMOKE_PRIVATE=nonce)
        config = base / 'bus.conf'
        config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=' + str(base) + '</listen><auth>EXTERNAL</auth><policy context="default">'
                          '<allow send_destination="*"/><allow own="*"/><allow receive_sender="*"/></policy></busconfig>')
        command = gui_sandbox.sandboxed(['dbus-run-session', '--config-file', str(config), '--', 'xvfb-run', '-a', '-s', '-screen 0 1440x900x24',
                                         sys.executable, str(pathlib.Path(__file__).resolve()), '--app', str(app), '--output', str(output),
                                         '--private-run', nonce], keep=[base, output])
        print('EXECUTED bwrap private /tmp/net/run-user, private D-Bus/Xvfb; child env from scratch; no owner desktop/session bus', flush=True)
        process = subprocess.Popen(command, cwd=ROOT, env=env, start_new_session=True)
        try:
            return process.wait(timeout=240)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=8)
            return 124


if __name__ == '__main__':
    raise SystemExit(main())
