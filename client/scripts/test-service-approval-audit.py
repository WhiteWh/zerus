#!/usr/bin/env python3
"""Run the native Settings read path against a real passive service and private SQL fixture."""
import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hgs', type=Path, required=True, help='privately built root CLI')
    args = parser.parse_args()
    hgs = args.hgs.resolve(strict=True)
    repo = Path(__file__).resolve().parents[2]
    root = Path(tempfile.mkdtemp(prefix='zerus-audit-', dir='/tmp'))
    for name in ['config', 'state', 'tmp', 'tmux', 'bin']:
        (root / name).mkdir(mode=0o700)
    env = {key: val for key, val in os.environ.items()
           if not key.startswith('HGS_') and key not in ('TMUX', 'TMUX_PANE', 'ZERUS_CLIENT_REPLAY')}
    env.update(HOME=str(root), USERPROFILE=str(root), TMPDIR=str(root / 'tmp'),
               TMUX_TMPDIR=str(root / 'tmux'), HGS_CONFIG_DIR=str(root / 'config'),
               HGS_STATE_DIR=str(root / 'state'), HGS_SELF='example', HGS_PEERS='')
    for name in ['ssh', 'tmux', 'systemctl', 'launchctl', 'schtasks']:
        denied = root / 'bin' / name
        denied.write_text('#!/bin/sh\nprintf called >> "$HOME/unexpected-tool"\nexit 99\n')
        denied.chmod(0o700)
    env['PATH'] = str(root / 'bin') + os.pathsep + env.get('PATH', '')
    relay = root / 'relay'
    private = {key: env[key] for key in ['HOME', 'USERPROFILE', 'TMPDIR', 'TMUX_TMPDIR',
               'HGS_CONFIG_DIR', 'HGS_STATE_DIR', 'HGS_SELF', 'HGS_PEERS', 'PATH']}
    relay.write_text('#!' + sys.executable + '\nimport os,sys\n'
                     + 'env={k:v for k,v in os.environ.items() if not k.startswith("HGS_") and k not in ("TMUX","TMUX_PANE")}\n'
                     + 'env.update(' + repr(private) + ')\n'
                     + 'os.execve(' + repr(str(hgs)) + ', [' + repr(str(hgs)) + '] + sys.argv[1:], env)\n')
    relay.chmod(0o700)
    audit = root / 'state' / 'approval-audit'
    audit.mkdir(mode=0o700)
    database = audit / 'journal.sqlite3'
    with sqlite3.connect(database) as db:
        db.executescript('''PRAGMA user_version=1;
            CREATE TABLE attempts(id INTEGER PRIMARY KEY, identity TEXT NOT NULL UNIQUE,
                request_id TEXT NOT NULL UNIQUE, at INTEGER NOT NULL, host TEXT NOT NULL,
                session TEXT NOT NULL, provider TEXT NOT NULL, prompt TEXT NOT NULL,
                question TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'prepared', detail TEXT NOT NULL DEFAULT '');
            CREATE INDEX attempts_session ON attempts(session,id DESC);
            CREATE INDEX attempts_host ON attempts(host,id DESC);
            CREATE INDEX attempts_host_session ON attempts(host,session,id DESC);''')
        question = json.dumps({'question': {'questions': [{'question': 'Service-owned question', 'body': 'Fixture body'}]}, 'answers': []})
        db.executemany('INSERT INTO attempts VALUES(?,?,?,?,?,?,?,?,?,?,?)',
                       [(n, f'fixture-{n}', str(uuid.uuid4()), 1800000000000, 'a.example',
                         'agent/project/fixture', 'claude', 'Service-owned question', question,
                         'approved', 'Service audit outcome') for n in range(1, 54)])
    db.close()
    database.chmod(0o600)
    before = database.read_bytes()
    process = None
    result = 1
    completed = False
    try:
        with (root / 'service.log').open('wb') as log:
            process = subprocess.Popen([str(hgs), 'desktop', 'serve'], env=env,
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        deadline = time.monotonic() + 15
        while True:
            if process.poll() is not None:
                raise RuntimeError('private service exited: ' + (root / 'service.log').read_text())
            status = subprocess.run([str(hgs), 'desktop', 'status', '--json'], env=env,
                                    capture_output=True, text=True, timeout=10, check=True)
            if json.loads(status.stdout)['running']:
                break
            if time.monotonic() > deadline:
                raise RuntimeError('private service readiness deadline')
            time.sleep(.02)
        test_env = {key: val for key, val in os.environ.items() if key not in ('HGS_SESSION', 'HGS_RUN_ID')}
        test_env['ZERUS_TEST_DESKTOP_AUDIT_RELAY'] = str(relay)
        result = subprocess.run(['cargo', '+1.99.1', 'test', '--locked', '--manifest-path',
                                 'client/src-tauri/Cargo.toml', '--lib',
                                 'desktop_service::audit::tests::real_service_owned_journal_reaches_the_settings_read_path',
                                 '--', '--exact', '--ignored', '--nocapture'], cwd=repo, env=test_env).returncode
        assert database.read_bytes() == before, 'journal database bytes changed'
        assert not (root / 'unexpected-tool').exists(), 'passive read launched a background tool'
        completed = result == 0
    finally:
        if process is not None and process.poll() is None:
            process.terminate()  # Only our original private service child.
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                print('Private service exit unconfirmed; retained at', root, file=sys.stderr)
                raise
        if completed:
            shutil.rmtree(root)
        else:
            print('Private audit fixture retained at', root, file=sys.stderr)
    return result


if __name__ == '__main__':
    sys.exit(main())
