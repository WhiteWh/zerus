#!/usr/bin/env python3
"""Actual-CLI Codex current-WAL child reconciliation with a private tmux agent.

The ordinary path positively reads a real SQLite catalog and native rollout.
The guarded ID Effort path must read that same current child from a private
main+WAL snapshot, without native writes, SQLite locks or sidecar mutations.
This is not official-agent qualification.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import time

import native_effort_id
import test_effort
import test_input

CHILD = '22222222-2222-4222-8222-222222222222'
TURN = '33333333-3333-4333-8333-333333333333'


def cleanup(fixture):
    until = time.monotonic() + 2
    while True:
        locks = [fixture.state / '.lock']
        sid = fixture.record.get('session_id')
        if sid:
            locks.append(fixture.state / 'native-input' / (sid + '.lock'))
        busy = False
        for path in locks:
            if not path.exists():
                continue
            with path.open('rb') as stream:
                try:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    fcntl.flock(stream, fcntl.LOCK_UN)
                except BlockingIOError:
                    busy = True
        if not busy:
            fixture.tearDown()
            fixture.doCleanups()
            return
        if time.monotonic() >= until:
            fixture.temp._finalizer.detach()
            raise RuntimeError('Keeper cleanup unknown; retained private fixture at ' + str(fixture.root))
        time.sleep(.02)


def rollout(path, parent, event, stamp):
    header = dict(type='session_meta', payload=dict(id=CHILD, parent_thread_id=parent))
    row = dict(type='event_msg', timestamp=stamp,
               payload=dict(type=event, turn_id=TURN, last_agent_message='Complete'))
    path.write_text(json.dumps(header) + '\n' + json.dumps(row) + '\n')


def signature(path):
    try:
        stat = path.stat()
        return stat.st_ino, stat.st_size, stat.st_mtime_ns, path.read_bytes()
    except FileNotFoundError:
        return None


def qualify(args):
    trace = shutil.which('strace')
    if not trace:
        raise RuntimeError('This Linux qualification requires strace; no skipped proof')
    hgs_sha256 = native_effort_id.image_digest(test_input.HGS)
    fixture = test_effort.EffortTransport(methodName='runTest')
    fixture.setUp()
    db = None
    try:
        home = fixture.root / '.codex'
        database = home / 'state_5.sqlite'
        child = home / 'rollout-child.jsonl'
        db = sqlite3.connect(database)
        assert db.execute('PRAGMA journal_mode=WAL').fetchone()[0] == 'wal'
        db.execute('CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT)')
        db.commit()
        db.execute('PRAGMA wal_checkpoint(TRUNCATE)')
        db.execute('INSERT INTO threads VALUES(?,?)', (CHILD, str(child)))
        db.commit()
        assert CHILD.encode() not in database.read_bytes()
        assert CHILD.encode() in (home / 'state_5.sqlite-wal').read_bytes()
        fixture.record.update(main_done=True, agent_home=str(home), subagents={CHILD: dict(
            name='Catalog child', state='working', started=1.0, updated=2.0)})
        fixture.write_record()
        rollout(child, fixture.conversation_id, 'task_complete', '1970-01-01T00:00:03Z')
        # Without the native catalog/rollout, the recorded working child would
        # block this ordinary transaction. It must really change the terminal.
        ordinary = fixture.change(fixture.payload('high'))
        fixture.assertEqual(ordinary['status'], 'confirmed')
        fixture.assertEqual(fixture.received(), b'\x1b,')
        fixture.record = json.loads(fixture.record_path.read_bytes())
        fixture.assertEqual(fixture.record['subagents'][CHILD]['state'], 'finished')
        image = args.test_image.resolve(strict=True)
        os.environ['ZERUS_PICKER_EFFORT_SEED_IMAGE'] = str(image)
        os.environ['ZERUS_PICKER_EFFORT_SEED_SHA256'] = native_effort_id.image_digest(image)
        record, path, legacy = native_effort_id.moved_record(fixture, fixture.name, fixture.record)
        fixture.record, fixture.record_path = record, path
        fixture.assertEqual(record['subagents'][CHILD]['state'], 'finished')
        # Native work has restarted while the authoritative saved row still
        # looks settled. Silently omitting reconciliation would be unsafe.
        rollout(child, fixture.conversation_id, 'task_started', '1970-01-01T00:00:04Z')
        observed = native_effort_id.inspect(fixture, fixture.name, record['session_id'])
        fixture.assertEqual(observed['subagent_active_count'], 1)
        fixture.assertEqual(observed['phase'], 'working')
        fixture.assertEqual(json.loads(path.read_bytes())['subagents'][CHILD]['state'], 'finished')
        before = {
            'record': path.read_bytes(),
            'main': signature(database),
            'wal': signature(home / 'state_5.sqlite-wal'),
            'shm': signature(home / 'state_5.sqlite-shm'),
            'received': fixture.received(),
        }
        sidecars = {suffix: (home / ('state_5.sqlite' + suffix)).exists()
                    for suffix in ('-wal', '-shm', '-journal')}
        request = fixture.payload('low')
        log = fixture.root / 'native-catalog-opens.log'
        command = [trace, '-f', '-yy', '-o', str(log), '-s', '2048',
                   '-e', 'trace=open,openat,openat2,creat,fcntl,flock,unlink,unlinkat,rename,renameat,renameat2',
                   str(test_input.HGS), 'effort', fixture.name,
                   '--session-id', record['session_id'], '--json']
        result = subprocess.run(command, input=json.dumps(request), env=fixture.env,
                                text=True, capture_output=True, timeout=25)
        fixture.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        fixture.assertIn('agent is busy or waiting for approval; wait for Ready or use Terminal',
                         result.stderr)
        fixture.assertLess(log.stat().st_size, 4 * 1024**2)
        prefix = str(home) + '/'
        native = [line for line in log.read_text().splitlines() if prefix in line]
        opens = [line for line in native if any(call in line for call in ('open(', 'openat(', 'openat2(', 'creat('))]
        fixture.assertTrue(opens, 'no raw native catalog read was observed')
        for line in opens:
            fixture.assertIn('O_RDONLY', line, line)
            fixture.assertFalse(any(flag in line for flag in ('O_WRONLY', 'O_RDWR', 'O_CREAT')), line)
        lock_calls = [line for line in native if 'F_SETLK' in line or 'F_OFD_SETLK' in line or 'flock(' in line]
        fixture.assertEqual(lock_calls, [], lock_calls)
        sidecar_mutations = [line for line in native
                             if any(call in line for call in ('unlink', 'rename'))]
        fixture.assertEqual(sidecar_mutations, [], sidecar_mutations)
        fixture.assertEqual(signature(database), before['main'])
        fixture.assertEqual(signature(home / 'state_5.sqlite-wal'), before['wal'])
        fixture.assertEqual(signature(home / 'state_5.sqlite-shm'), before['shm'])
        fixture.assertEqual(fixture.received(), before['received'])
        fixture.assertEqual(json.loads(path.read_bytes())['subagents'][CHILD]['state'], 'finished')
        observed_after = native_effort_id.inspect(fixture, fixture.name, record['session_id'])
        fixture.assertEqual(observed_after['subagent_active_count'], 1)
        fixture.assertEqual(observed_after['phase'], 'working')
        fixture.assertEqual({suffix: (home / ('state_5.sqlite' + suffix)).exists()
                             for suffix in sidecars}, sidecars)
        fixture.assertFalse((fixture.state / 'effort_receipts' / (request['request_id'] + '.json')).exists())
        fixture.assertFalse(legacy.exists())
        fixture.assertEqual(native_effort_id.image_digest(test_input.HGS), hgs_sha256)
        shutil.copy(log, args.output.parent / 'native-catalog-opens.log')
        return dict(boundary='actual CLI; private scripted tmux/SQLite fixture; not official native',
                    hgs_sha256=hgs_sha256,
                    seed_sha256=native_effort_id.image_digest(image),
                    ordinary_native_child_reconciliation=True,
                    current_wal_child_reconciled=True,
                    native_readiness_refusal='busy',
                    native_catalog_raw_reads=len(opens), native_sqlite_locks=0,
                    native_catalog_write_opens=0, native_sidecar_mutations=0,
                    new_keys=0, new_receipts=0,
                    source_bytes_and_sidecar_membership_unchanged=True)
    finally:
        if db is not None:
            db.close()
        cleanup(fixture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--test-image', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = qualify(args)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
