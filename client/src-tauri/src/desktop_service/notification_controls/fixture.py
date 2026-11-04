#!/usr/bin/env python3
"""Private notification-control transport; native evidence comes from the real cache."""
import hashlib
import json
import os
from pathlib import Path
import socket
import sys
import time
import uuid

root = Path(os.path.abspath(__file__)).parent
args = sys.argv[1:]
config = json.loads((root / 'fixture.json').read_text())
abi = 'desktop-notification-controls-v1'
journal = '00000000-0000-4000-8000-000000000020'
value = json.loads(sys.stdin.readline()) if args != ['--help'] else None
with (root / 'calls').open('a') as log:
    log.write(json.dumps({'args': args, 'value': value, 'pid': os.getpid()}) + '\n')


def wait(name):
    until = time.monotonic() + 20
    while not (root / name).exists():
        assert time.monotonic() < until, name
        time.sleep(.005)


def fingerprint(scope, command):
    # Match the closed Rust Command's serialization, including after a generic
    # read request's serde_json::Value has reordered its object keys.
    command = {key: command[key] for key in ['kind', 'request_id', 'journal', 'revision', 'preferences']}
    raw = json.dumps([scope['configuration'], command], separators=(',', ':'), ensure_ascii=False)
    return hashlib.sha256(raw.encode()).hexdigest()


if args == ['--help']:
    if not config.get('missing_frontend_abi'):
        print('Desktop frontend request ABI: desktop-ui-request-v1')
    if not config.get('missing_abi'):
        print('Notification controls ABI: ' + abi)
    sys.exit(0)

if args == ['desktop', 'frontend-request', '--json']:
    request = value['request']
    scope, control = request['scope'], request['control']
    assert request['op'] == 'notification_control'
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(2)
        stream.connect(str(root / 'native-cache.sock'))
        stream.sendall(('desktop-ui-request-v1 ' + value['attempt'] + '\n').encode())
        reply = json.loads(stream.makefile('rb').readline(8193))
    evidence = reply['request']
    assert evidence['child_pid'] == os.getpid()
    assert evidence['attempt'] == value['attempt']
    assert evidence['request_id'] == control['request_id']
    assert evidence['fingerprint'] == hashlib.sha256(json.dumps(request, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()
    native = evidence['notification']
    assert native['scope'] == scope and native['mode'] == 'service'
    with (root / 'evidence').open('a') as log:
        log.write(json.dumps(native) + '\n')
    if control['kind'] == 'focus':
        focus = native['focus']
        if config.get('hold_focus') and focus['state'] == 'foreground' and not (root / 'focus-held').exists():
            (root / 'focus-held').touch()
            # Return from the original process, but retain its pipes in our
            # own bounded descendant. A cache invalidation is not a pipe EOF.
            if os.fork() == 0:
                wait('release-focus')
                os._exit(0)
            sys.exit(1)
        print(json.dumps(dict(abi=abi, request_id=control['request_id'], scope=scope,
                              observation=focus, ttl_ms=5000 if focus['state'] == 'foreground' else 0)))
    else:
        if config.get('lose_configure') and not (root / 'allow-save').exists():
            sys.exit(1)
        receipt = dict(abi=abi, request_id=control['request_id'], scope=scope,
                       fingerprint=fingerprint(scope, control), journal=control['journal'] or journal,
                       revision=control['revision'] + 1, preferences=control['preferences'],
                       durable=True, outcome={'state': 'saved'})
        if config.get('refuse_configure'):
            receipt.update(journal=control['journal'], revision=control['revision'], durable=False,
                           outcome={'state': 'refused', 'code': 'preferences_revision_mismatch'})
        else:
            (root / 'saved').write_text(json.dumps(receipt))
        print(json.dumps(receipt))
elif value['op'] == 'notification_preferences_status':
    scope = value['scope']
    saved = json.loads((root / 'saved').read_text()) if (root / 'saved').exists() else None
    current = dict(abi=abi, scope=scope, journal=journal, revision=1,
                   configured=True, preferences={'optional_questions': False})
    if config.get('unconfigured') and not saved:
        current.update(journal=None, revision=0, configured=False)
    if saved:
        current.update(journal=saved['journal'], revision=saved['revision'], preferences=saved['preferences'])
    if config.get('current_newer'):
        current.update(revision=8, preferences={'optional_questions': False})
    if config.get('invalid_status'):
        current['scope'] = dict(scope, service_epoch=str(uuid.uuid4()))
    print(json.dumps(current))
else:
    assert value['op'] == 'notification_preferences_result'
    query = value['query']
    command, scope = query['original'], query['scope']
    saved = json.loads((root / 'saved').read_text()) if (root / 'saved').exists() else None
    print(json.dumps(dict(abi=abi, request_id=query['request_id'], scope=scope,
                          original_request_id=command['request_id'], fingerprint=fingerprint(scope, command),
                          receipt=saved, definitive_not_applied=bool(config.get('definitive')))))
