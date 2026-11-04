#!/usr/bin/env python3
"""Launch a canonical Linux client with the real CLI through a private user unit.

Uses the maintained updater, a fresh HOME/XDG/config/socket, private D-Bus and
Xvfb. Does not register/replace either installed user service or use the owner
screen. Requires a running systemd user manager, Xvfb, xwininfo and dbus-run-session.
Build the app with tauri.switch.linux.conf.json and tauri/custom-protocol first.
Example: python3 client/scripts/linux-startup-smoke.py --app BUILD/zerus-client \
    --hgs BUILD/hgs --output FRESH_PRIVATE_DIRECTORY [--webkit-prefix PREFIX]
A missing prerequisite is a failure, never a successful launch qualification.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def worker(args):
    base = args.output
    env = {k: os.environ[k] for k in ('PATH', 'LANG', 'DISPLAY', 'XAUTHORITY', 'DBUS_SESSION_BUS_ADDRESS') if k in os.environ}
    for key, name in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'),
                      ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'),
                      ('XDG_STATE_HOME', 'xdg-state'), ('XDG_RUNTIME_DIR', 'runtime'),
                      ('HGS_CONFIG_DIR', 'hgs-config'), ('HGS_STATE_DIR', 'state'),
                      ('TMPDIR', 'tmp'), ('TMUX_TMPDIR', 'tmux')]:
        directory = base / name
        directory.mkdir(mode=0o700)
        env[key] = str(directory)
    env.update(GDK_BACKEND='x11', GSETTINGS_BACKEND='memory', NO_AT_BRIDGE='1',
               WEBKIT_DISABLE_DMABUF_RENDERER='1', ZERUS_CLIENT_HGS=str(args.hgs))
    help_result = subprocess.run([str(args.hgs), '--help'], env=env, capture_output=True, timeout=10, check=True)
    for marker in [b'Desktop frontend request ABI: desktop-ui-request-v1',
                   b'Desktop ownership ABI: desktop-ownership-v1']:
        assert marker in help_result.stdout, 'real CLI lacks the required ABI'
    installed = Path(env['HOME']) / '.local/bin/hgs-tray'
    # Fresh private target: updater verifies the build and installs without start.
    staged = subprocess.run([sys.executable, str(ROOT / 'tray/update-installed.py'),
                             str(args.app), str(installed)], env=env,
                            capture_output=True, timeout=30, check=True)
    (base / 'stage.log').write_bytes(staged.stdout + staged.stderr)
    info = json.loads(subprocess.check_output([str(installed), '--build-info'], env=env, timeout=10))
    assert info['CFBundleIdentifier'] == 'com.hgdev.hgs-tray', 'use the canonical switch build'
    assert info['sourceDirty'] is False, 'native authentication requires a clean committed GUI build'
    if args.webkit_prefix:
        launcher = load(ROOT / 'tray/linux-launcher.py', 'local_launcher')
        assert launcher.available(args.webkit_prefix), 'incomplete private WebKit prefix'
        setting = installed.parent.parent / launcher.SETTING
        setting.parent.mkdir(parents=True)
        setting.write_text(json.dumps({'version': 1, 'prefix': str(args.webkit_prefix)}))
        original = dict(os.environ)
        try:
            os.environ.clear()
            os.environ.update(env)
            env = launcher.environment(installed.parent.parent, installed)
        finally:
            os.environ.clear()
            os.environ.update(original)
        assert launcher.ENV_MARKER in env, 'patched launcher environment was not applied'
    diagnostic = base / 'xdg-state/hgs/client-startup-refusal.json'
    window = False
    observed = None
    with (base / 'app.log').open('wb') as log:
        app = subprocess.Popen([str(installed), '--sessions'], env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            until = time.monotonic() + 25
            while time.monotonic() < until and app.poll() is None:
                if diagnostic.exists():
                    assert diagnostic.stat().st_size <= 512
                    observed = json.loads(diagnostic.read_bytes())
                    break
                tree = subprocess.run(['xwininfo', '-root', '-tree'], env=env, capture_output=True, timeout=3, check=True)
                if b'"Zerus Next"' in tree.stdout:
                    window = True
                    break
                time.sleep(.1)
        finally:
            # Original private child only. Normal SIGTERM invokes the client's
            # draft/worker flush; no PID/name lookup or process-group termination.
            if app.poll() is None:
                app.terminate()
            try:
                app.wait(timeout=15)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait(timeout=5)
                raise AssertionError('private original client did not quit normally')
    result = {'window': window, 'exit': app.returncode, 'refusal': observed,
              'patched_webkit': bool(args.webkit_prefix)}
    (base / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    if args.expect_refusal:
        assert not window and observed and observed['code'] == args.expect_refusal, result
        assert observed['step'] == 'native_takeover.frontend_request', result
        assert diagnostic.stat().st_mode & 0o777 == 0o600
    else:
        assert window and observed is None and app.returncode == 0, result
        assert not (base / 'tmp/hgs-tray').exists(), 'original canonical socket did not drain'
        # Never-service NativeAbsent is an authenticated no-op, not a new store.
        assert not list((base / 'state').rglob('*.sqlite3')), 'native absent created a service store'
    print(json.dumps(result), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--hgs', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--webkit-prefix', type=Path)
    parser.add_argument('--expect-refusal')
    parser.add_argument('--worker', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        worker(args)
        return
    assert sys.platform == 'linux' and os.geteuid() != 0, 'run as the ordinary Linux login user'
    for tool in ['systemd-run', 'systemctl', 'dbus-run-session', 'xvfb-run', 'xwininfo']:
        assert shutil.which(tool), f'missing prerequisite: {tool}'
    args.app = args.app.resolve(strict=True)
    args.hgs = args.hgs.resolve(strict=True)
    args.output = args.output.absolute()
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    command = [str(Path(__file__).resolve()), '--worker', '--app', str(args.app),
               '--hgs', str(args.hgs), '--output', str(args.output)]
    if args.webkit_prefix:
        command += ['--webkit-prefix', str(args.webkit_prefix.resolve(strict=True))]
    if args.expect_refusal:
        command += ['--expect-refusal', args.expect_refusal]
    # A genuine user-manager parent is essential. Starting below the agent and
    # clearing its variables would correctly remain an agent-descended refusal.
    with (args.output / 'unit.log').open('wb') as log:
        subprocess.run(['systemd-run', '--user', '--wait', '--collect',
                        '--unit=zerus-private-startup-' + uuid.uuid4().hex,
                        '--property=RuntimeMaxSec=90', '--property=TimeoutStopSec=10',
                        'dbus-run-session', '--', 'xvfb-run', '-a', '-s',
                        '-screen 0 1440x900x24', sys.executable, *command],
                       stdout=log, stderr=subprocess.STDOUT, timeout=110, check=True)
    print((args.output / 'result.json').read_text())


if __name__ == '__main__':
    main()
