#!/usr/bin/env python3
"""Refuse a rollback-mode Codex catalog through the actual guarded ID Effort CLI.

A private external writer spills an uncommitted child-path UPDATE into the main
database and holds its hot rollback journal. The guarded picker must refuse
without native keys, receipts, SQLite locks or source/sidecar mutations.
This is not official-agent qualification.
"""
import argparse
import contextlib
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
    writer = None
    try:
        home = fixture.root / '.codex'
        database = home / 'state_5.sqlite'
        child = home / 'rollout-child.jsonl'
        with contextlib.closing(sqlite3.connect(database)) as db:
            db.execute('CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT)')
            db.execute('INSERT INTO threads VALUES(?,?)', (CHILD, str(child)))
            db.commit()
        fixture.record.update(main_done=True, agent_home=str(home), subagents={CHILD: dict(
            name='Catalog child', state='working', started=1.0, updated=2.0)})
        fixture.write_record()
        rollout(child, fixture.conversation_id, 'task_complete', '1970-01-01T00:00:03Z')
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
        rollout(child, fixture.conversation_id, 'task_started', '1970-01-01T00:00:04Z')
        observed = native_effort_id.inspect(fixture, fixture.name, record['session_id'])
        fixture.assertEqual(observed['subagent_active_count'], 1)
        fixture.assertEqual(observed['phase'], 'working')
        fixture.assertEqual(json.loads(path.read_bytes())['subagents'][CHILD]['state'], 'finished')
        ended = home / 'rollout-ended.jsonl'
        rollout(ended, fixture.conversation_id, 'task_complete', '1970-01-01T00:00:05Z')
        writer = sqlite3.connect(database)
        assert writer.execute('PRAGMA journal_mode=DELETE').fetchone()[0] == 'delete'
        writer.execute('CREATE TABLE padding(id INTEGER PRIMARY KEY, value TEXT)')
        writer.executemany('INSERT INTO padding VALUES(?,?)', ((i, 'x'*200) for i in range(1000)))
        writer.commit()
        writer.execute('PRAGMA cache_size=1')
        writer.execute('PRAGMA cache_spill=ON')
        writer.execute('BEGIN IMMEDIATE')
        writer.execute('UPDATE threads SET rollout_path=? WHERE id=?', (str(ended), CHILD))
        writer.execute('UPDATE padding SET value=?', ('y'*200,))
        assert str(ended).encode() in database.read_bytes(), 'dirty target page has not spilled'
        assert (home / 'state_5.sqlite-journal').is_file()
        before = {
            'record': path.read_bytes(),
            'main': signature(database),
            'wal': signature(home / 'state_5.sqlite-wal'),
            'shm': signature(home / 'state_5.sqlite-shm'),
            'journal': signature(home / 'state_5.sqlite-journal'),
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
        args.output.write_text(json.dumps(dict(
            guarded_returncode=result.returncode, guarded_stdout=result.stdout, guarded_stderr=result.stderr,
            keys_before=before['received'].hex(), keys_after=fixture.received().hex(),
            committed_rollout=str(child), uncommitted_rollout=str(ended),
            source_main_unchanged=signature(database)==before['main'],
            source_journal_unchanged=signature(home / 'state_5.sqlite-journal')==before['journal']), indent=2))
        shutil.copy(log, args.output.parent / 'rollback-cli-trace.log')
        fixture.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        fixture.assertIn('ID Effort cannot verify this Codex child catalog; use Terminal',
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
        fixture.assertEqual(signature(home / 'state_5.sqlite-journal'), before['journal'])
        fixture.assertEqual(fixture.received(), before['received'])
        fixture.assertEqual(json.loads(path.read_bytes())['subagents'][CHILD]['state'], 'finished')
        fixture.assertEqual({suffix: (home / ('state_5.sqlite' + suffix)).exists()
                             for suffix in sidecars}, sidecars)
        fixture.assertFalse((fixture.state / 'effort_receipts' / (request['request_id'] + '.json')).exists())
        fixture.assertFalse(legacy.exists())
        fixture.assertEqual(native_effort_id.image_digest(test_input.HGS), hgs_sha256)
        writer.rollback()
        row = writer.execute('SELECT rollout_path FROM threads WHERE id=?', (CHILD,)).fetchone()
        fixture.assertEqual(row, (str(child),))
        writer.close()
        writer = None
        observed_after = native_effort_id.inspect(fixture, fixture.name, record['session_id'])
        fixture.assertEqual(observed_after['subagent_active_count'], 1)
        fixture.assertEqual(observed_after['phase'], 'working')
        shutil.copy(log, args.output.parent / 'rollback-native-catalog-opens.log')
        return dict(boundary='actual CLI; private scripted tmux/SQLite fixture; not official native',
                    hgs_sha256=hgs_sha256,
                    seed_sha256=native_effort_id.image_digest(image),
                    ordinary_native_child_reconciliation=True,
                    rollback_uncommitted_refused=True,
                    native_catalog_refusal='unverified',
                    native_catalog_raw_reads=len(opens), native_sqlite_locks=0,
                    native_catalog_write_opens=0, native_sidecar_mutations=0,
                    new_keys=0, new_receipts=0,
                    source_bytes_and_sidecar_membership_unchanged=True)
    finally:
        if writer is not None:
            writer.rollback()
            row = writer.execute('SELECT rollout_path FROM threads WHERE id=?', (CHILD,)).fetchone()
            assert row == (str(child),), row
            writer.close()
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
