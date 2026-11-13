"""Shared actual-CLI ID Effort contract for maintained real native fixtures.

The runner supplies a separately built, hash-pinned Rust test image for the
isolated owner-transfer setup. Every Effort request uses the production CLI.
The native executable/profile/terminal remain owned by the provider fixture.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import stat
import subprocess
import sys
import time
import uuid

import test_input

SEED_METHOD = 'state::effort::picker_scope::tests::effort::seed_actual_effort_record'


def bounded_bytes(path, limit):
    flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0)
    with os.fdopen(os.open(path, flags), 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
            raise ValueError('native fixture snapshot is not a bounded regular file')
        raw = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
        if (len(raw) != before.st_size or len(raw) > limit or
                (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) !=
                (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)):
            raise ValueError('native fixture snapshot changed while reading')
        return raw


def image_digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def moved_record(case, name, record, *, existing_machine=False, kind='moved'):
    """Move this exact saved legacy fixture through the real transition code."""
    image = Path(os.environ['ZERUS_PICKER_EFFORT_SEED_IMAGE']).resolve(strict=True)
    expected = os.environ['ZERUS_PICKER_EFFORT_SEED_SHA256']
    case.assertEqual(image_digest(image), expected)
    root = Path(case.root).resolve()
    state = Path(case.env['HGS_STATE_DIR']).resolve()
    case.assertEqual(Path(case.env['HOME']).resolve(), root)
    case.assertEqual(state.parent, root)
    state.chmod(0o700)
    case.assertEqual(record['name'], name)
    case.assertIn(record['agent'], ('claude', 'codex', 'kimi'))
    original = {key: record.get(key) for key in (
        'session_id', 'run_id', 'conversation_id', 'pid', 'process_start',
        'supervisor', 'terminal_name', 'agent', 'agent_home')}
    source = root / 'native-effort-record.json'
    source.write_text(json.dumps(record))
    env = dict(case.env, ZERUS_PICKER_EFFORT_SEED='1',
               ZERUS_PICKER_EFFORT_RECORD=str(source), ZERUS_PICKER_EFFORT_KIND=kind)
    if existing_machine:
        # Public read/initialization before the owner-transfer fixture, shared
        # verbatim by the base-main CLI. The Send keeper only reads this ID.
        hello = subprocess.run([str(test_input.HGS), 'swarm', 'hello'], env=case.env,
                               stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=10)
        case.assertEqual(hello.returncode, 0, hello.stdout + hello.stderr)
        node = json.loads(hello.stdout)['node_id']
        case.assertEqual(str(uuid.UUID(node)), node)
    seeded = subprocess.run([str(image), '--exact', SEED_METHOD, '--ignored', '--test-threads=1'],
                            env=env, stdin=subprocess.DEVNULL, capture_output=True,
                            text=True, timeout=15)
    case.assertEqual(seeded.returncode, 0, seeded.stdout + seeded.stderr)
    case.assertEqual(image_digest(image), expected)
    current = json.loads(source.read_bytes())
    sid = current['session_id']
    case.assertEqual(str(uuid.UUID(sid)), sid)
    case.assertEqual(current['layout'], 'id')
    if original['session_id']:
        case.assertEqual(sid, original['session_id'])
    for key, value in original.items():
        if key != 'session_id' and value is not None:
            case.assertEqual(current.get(key), value, key)
    path = state / 'sessions' / (sid + '.json')
    case.assertEqual(json.loads(path.read_bytes()), current)
    legacy = state / (hashlib.sha256(name.encode()).hexdigest() + '.json')
    case.assertFalse(legacy.exists(), 'the original legacy record was not retained by the transition')
    return current, path, legacy


def pending_then_message(case, name, record, path, model, effort, text, attempts):
    """Seed one exact scheduled intent, then use only the public production send.

    The caller owns this private native record and verifies inference/defaults.
    This also reaches unprimed Kimi without fabricating or priming its CID.
    """
    pending = str(uuid.uuid4())
    original = {key: record.get(key) for key in (
        'session_id', 'run_id', 'conversation_id', 'pid', 'process_start',
        'supervisor', 'terminal_name', 'agent', 'agent_home')}
    record['pending_settings'] = dict(
        request_id=pending, run_id=record['run_id'],
        conversation_id=record.get('conversation_id'), model=model, effort=effort,
        receipt_name=name, receipt_run_id=record['run_id'])
    Path(path).write_text(json.dumps(record))
    directory = Path(case.env['HGS_STATE_DIR']) / 'settings_receipts'
    directory.mkdir(mode=0o700, exist_ok=True)
    scheduled = directory / (pending + '.json')
    scheduled.write_text(json.dumps(dict(
        request_id=pending, name=name, run_id=record['run_id'],
        conversation_id=record.get('conversation_id'), status='scheduled',
        model=model, effort=effort, scope='session')))
    scheduled.chmod(0o600)
    request = dict(request_id=str(uuid.uuid4()), expected_run_id=record['run_id'],
                   expected_conversation_id=record.get('conversation_id') or '', text=text)
    raw = '{\n ' + json.dumps(request, separators=(',', ':'))[1:-1] + '\n}\n'
    command = [str(test_input.HGS), 'send', name, '--session-id', record['session_id'], '--json']
    case.env['ZERUS_PENDING_SEND_RECEIPT'] = str(Path(case.env['HGS_STATE_DIR']) /
                                               'input_receipts' / (request['request_id'] + '.json'))
    before = attempts()
    started = time.monotonic()
    result = subprocess.run(command, env=case.env, input=raw, text=True, capture_output=True, timeout=30)
    if result.returncode:
        diagnostic = dict(request_id=request['request_id'],
                          seconds=round(time.monotonic() - started, 3))
        for label, source in [('record', Path(path)), ('settings', scheduled),
                              ('input', Path(case.env['ZERUS_PENDING_SEND_RECEIPT']))]:
            try:
                value = json.loads(bounded_bytes(source, 1 << 20))
                diagnostic[label] = {key: value.get(key) for key in (
                    'status', 'request_id', 'digest', 'session_id', 'run_id', 'conversation_id',
                    'model', 'effort', 'pending_settings', 'settings_unconfirmed_at',
                    'input_pending_at', 'input_pending_request')}
            except Exception as error:
                diagnostic[label] = dict(snapshot_error=str(error)[:500])
        try:
            diagnostic['attempts_tail'] = attempts()[-8192:].decode(errors='replace')
            screen = subprocess.run([str(Path(case.bin) / 'tmux'), 'capture-pane', '-p', '-J',
                                     '-t', record['pane']], env=case.env,
                                    stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
            diagnostic['screen'] = screen.stdout[-8192:].decode(errors='replace')
            diagnostic['screen_exit'] = screen.returncode
        except Exception as error:
            diagnostic['screen_error'] = str(error)[:500]
        print('ZERUS_NATIVE_PENDING_SEND_FAILURE=' + json.dumps(diagnostic), flush=True)
    case.assertEqual(result.returncode, 0, result.stdout + result.stderr)
    receipt = json.loads(result.stdout)
    case.assertEqual(receipt['status'], 'submitted')
    case.assertEqual((receipt['request_id'], receipt['digest']),
                     (request['request_id'], hashlib.sha256(raw.encode()).hexdigest()))
    case.assertEqual((receipt['text'], receipt['run_id'], receipt['conversation_id']),
                     (text, request['expected_run_id'], request['expected_conversation_id']))
    case.assertEqual(json.loads(scheduled.read_bytes())['status'], 'applied')
    current = json.loads(Path(path).read_bytes())
    case.assertNotIn('pending_settings', current)
    case.assertNotIn('settings_unconfirmed_at', current)
    case.assertEqual((current['model'], current['effort']), (model, effort))
    for key, value in original.items():
        if key != 'conversation_id':
            case.assertEqual(current.get(key), value, key)
    # The production first-message hook may now establish the real CID.
    if original['conversation_id']:
        case.assertEqual(current.get('conversation_id'), original['conversation_id'])
    after = attempts()
    case.assertNotEqual(after, before)
    observed = [json.loads(line) for line in bounded_bytes(
        Path(case.root) / 'native-pending-outer-receipts.jsonl', 1 << 20).splitlines()]
    case.assertTrue(observed, 'the native case must enter the new pending Send lane')
    case.assertEqual(observed[0]['receipt'], dict(
        status='in_progress', request_id=request['request_id'], digest=hashlib.sha256(raw.encode()).hexdigest()))
    repeated = subprocess.run(command, env=case.env, input=raw, text=True, capture_output=True, timeout=30)
    case.assertEqual(repeated.returncode, 0, repeated.stdout + repeated.stderr)
    case.assertEqual(json.loads(repeated.stdout), receipt)
    case.assertEqual(attempts(), after, 'replayed pending/message request typed twice')
    changed = subprocess.run(command, env=case.env, input=raw + ' ', text=True, capture_output=True, timeout=30)
    case.assertNotEqual(changed.returncode, 0)
    case.assertIn('already used', changed.stderr)
    case.assertEqual(attempts(), after)
    print('ZERUS_NATIVE_PENDING_SEND=' + json.dumps(dict(
        case=case.id(), agent=record['agent'], request_id=request['request_id'],
        pending_id=pending, hgs_sha256=image_digest(test_input.HGS),
        seed_sha256=os.environ['ZERUS_PICKER_EFFORT_SEED_SHA256'],
        seconds=round(time.monotonic() - started, 3), submitted=True,
        empty_conversation=not original['conversation_id'], replay_typed=False)), flush=True)
    return receipt


def key_log(case, existing=None):
    """Observe actual attempts while forwarding to the unchanged private tmux."""
    log = Path(existing) if existing is not None else Path(case.root) / 'native-effort-keys.jsonl'

    def attempts():
        try:
            return bounded_bytes(log, 1 << 20)
        except FileNotFoundError:
            return b''

    if existing is not None:
        # Claude already observes the real sends. Reuse that raw observer;
        # another Python wrapper on every read would distort the native budget.
        return attempts
    original = Path(case.bin) / 'tmux-native-effort-original'
    current = Path(case.bin) / 'tmux'
    current.rename(original)
    script = Path(case.root) / 'native-effort-tmux.py'
    script.write_text('import json,os,pathlib,subprocess,sys\n'
                      'args=sys.argv[1:]\n'
                      "if args[:1]==['send-keys']:\n"
                      ' with pathlib.Path(' + repr(str(log)) + ").open('a') as file:\n"
                      "  file.write(json.dumps(args)+'\\n')\n"
                      " if os.environ.get('ZERUS_PENDING_SEND_RECEIPT'):\n"
                      "  path=pathlib.Path(os.environ['ZERUS_PENDING_SEND_RECEIPT'])\n"
                      "  value=json.loads(path.read_bytes()) if path.exists() else {}\n"
                      "  receipt={key:value.get(key) for key in ('status','request_id','digest')}\n"
                      '  with pathlib.Path(' + repr(str(Path(case.root) / 'native-pending-outer-receipts.jsonl')) + ").open('a') as file:\n"
                      "   file.write(json.dumps(dict(args=args,receipt=receipt))+'\\n')\n"
                      'raise SystemExit(subprocess.call([' + repr(str(original)) + ']+args))\n')
    # Only keys need this Python observer. Starting another interpreter for
    # every capture/probe can consume the keeper's original finite budget on
    # macOS; preserve the retained wrapper's exact read path and any observer
    # it already supplies, including its argv and inherited streams.
    case.script('tmux', '#!/bin/sh\nif [ "$1" != send-keys ]; then exec ' +
                shlex.quote(str(original)) + ' "$@"; fi\nexec ' + shlex.quote(sys.executable) + ' ' +
                shlex.quote(str(script)) + ' "$@"\n')

    return attempts


def inspect(case, name, sid):
    result = subprocess.run([str(test_input.HGS), 'inspect', name, '--session-id', sid],
                            env=case.env, text=True, capture_output=True, timeout=30)
    case.assertEqual(result.returncode, 0, result.stdout + result.stderr)
    return json.loads(result.stdout)


def change_and_replay(case, name, record, path, effort, attempts):
    sid = record['session_id']
    request = dict(request_id=str(uuid.uuid4()), expected_run_id=record['run_id'],
                   expected_conversation_id=record['conversation_id'], effort=effort)
    raw = ('{\n  ' + json.dumps(request, separators=(',', ':'))[1:-1] + '\n}\n')
    command = [str(test_input.HGS), 'effort', name, '--session-id', sid, '--json']

    def call(body):
        return subprocess.run(command, env=case.env, input=body, text=True,
                              capture_output=True, timeout=30)

    before = attempts()
    started = time.monotonic()
    result = call(raw)
    if result.returncode:
        diagnostic = dict(request_id=request['request_id'], seconds=round(time.monotonic() - started, 3))
        for label, source in [('record', path), ('receipt', Path(case.env['HGS_STATE_DIR']) /
                              'effort_receipts' / (request['request_id'] + '.json'))]:
            try:
                value = json.loads(bounded_bytes(source, 64 << 10))
                diagnostic[label] = {key: value.get(key) for key in (
                    'status', 'request_id', 'digest', 'session_id', 'run_id', 'conversation_id',
                    'model', 'effort', 'settings_unconfirmed_at')}
            except Exception as error:
                diagnostic[label] = dict(snapshot_error=str(error)[:500])
        try:
            diagnostic['attempts_tail'] = attempts()[-8192:].decode(errors='replace')
        except Exception as error:
            diagnostic['attempts_tail'] = dict(snapshot_error=str(error)[:500])
        print('ZERUS_NATIVE_ID_EFFORT_FAILURE=' + json.dumps(diagnostic), flush=True)
    case.assertEqual(result.returncode, 0, result.stdout + result.stderr)
    receipt = json.loads(result.stdout)
    case.assertEqual(receipt['status'], 'confirmed')
    case.assertEqual((receipt['name'], receipt['scope']), (name, 'session'))
    case.assertEqual(receipt['request_id'], request['request_id'])
    case.assertEqual(receipt['digest'], hashlib.sha256(raw.encode()).hexdigest())
    case.assertEqual((receipt['run_id'], receipt['conversation_id'], receipt['effort']),
                     (record['run_id'], record['conversation_id'], effort))
    after = attempts()
    case.assertNotEqual(after, before, 'the native effort transaction delivered no keys')
    repeated = call(raw)
    case.assertEqual(repeated.returncode, 0, repeated.stdout + repeated.stderr)
    case.assertEqual(json.loads(repeated.stdout), receipt)
    case.assertEqual(attempts(), after, 'the same request typed into the native agent twice')
    changed = call(raw + ' ')
    case.assertNotEqual(changed.returncode, 0, changed.stdout)
    case.assertIn('already used', changed.stderr)
    case.assertEqual(attempts(), after, 'a changed raw request reached the native agent')
    current = json.loads(path.read_bytes())
    for key in ('session_id', 'run_id', 'conversation_id', 'pid', 'process_start',
                'supervisor', 'terminal_name', 'agent', 'agent_home'):
        case.assertEqual(current.get(key), record.get(key), key)
    case.assertEqual(current['effort'], effort)
    case.assertNotIn('settings_unconfirmed_at', current)
    print('ZERUS_NATIVE_ID_EFFORT=' + json.dumps(dict(
        case=case.id(), agent=record['agent'], kind='LegacyMoved',
        request_id=request['request_id'], raw_sha256=receipt['digest'],
        hgs_sha256=image_digest(test_input.HGS),
        seed_sha256=os.environ['ZERUS_PICKER_EFFORT_SEED_SHA256'],
        confirmed=True, duplicate_typed=False, changed_raw_refused=True)))
    return current
