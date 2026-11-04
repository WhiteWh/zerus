#!/usr/bin/env python3
"""Private Staged-report fixture. No service, agent, source mutation or effects."""
import hashlib
import json
import os
from pathlib import Path
import sys

root = Path(os.path.abspath(__file__)).parent
args = sys.argv[1:]
configuration = 'a' * 64
if args == ['--help']:
    print('Desktop import ABI: desktop-import-v1')
elif args == ['desktop', 'status', '--json']:
    print(json.dumps({'abi': 'desktop-v1', 'configuration': configuration}))
elif args[:2] == ['desktop', 'import-status']:
    if (root / 'mode').read_text() == 'lookup_error':
        raise SystemExit(1)
    target = args[-1]
    path = root / 'committed.json'
    receipt = json.loads(path.read_text()) if path.exists() else None
    answer = {'abi': 'desktop-import-v1', 'configuration': configuration,
              'import_id': target, 'record_present': receipt is not None}
    if receipt:
        assert receipt['import_id'] == target
        for key in ['evidence', 'pages', 'import_record']:
            answer[key] = receipt[key]
    print(json.dumps(answer))
elif args == ['desktop', 'import-evidence', '--json']:
    with (root / 'publications').open('a') as log:
        log.write('one attempt\n')
    pipe = sys.stdin.buffer
    start = json.loads(pipe.readline(4097))
    total = snapshots = pages = 0
    record = raw = None
    mode = (root / 'mode').read_text()
    while True:
        line = pipe.readline(4097)
        if not line:
            break
        header = json.loads(line)
        length = header.get('bytes', header.get('reference', {}).get('bytes'))
        assert 0 < length <= 1 << 20
        raw = pipe.read(length)
        assert len(raw) == length and pipe.read(1) == b'\n'
        if header['t'] == 'record':
            total += length
            record = json.loads(raw)
            assert not pipe.read(1)
            break
        total += length
        assert total <= 64 << 20
        snapshots += header['t'] == 'snapshot'
        pages += header['t'] == 'page'
        if mode == 'partial':
            raise SystemExit(1)
    if record is None:
        raise SystemExit(1)
    receipt = {'abi': 'desktop-import-v1', 'published': True,
               'evidence': 'staged', 'configuration': configuration,
               'import_id': start['import_id'],
               'import_record': {'version': 1, 'sha256': hashlib.sha256(raw).hexdigest()},
               'bytes': total, 'snapshots': snapshots, 'pages': pages}
    if mode == 'busy':
        print(json.dumps({key: receipt[key] for key in ['abi', 'configuration', 'import_id', 'import_record']} |
                         {'published': False, 'code': 'handoff_busy', 'this_attempt_not_committed': True}))
    elif mode == 'lost':
        pass
    elif mode == 'mismatch':
        receipt['import_record']['sha256'] = 'f' * 64
        print(json.dumps(receipt))
    else:
        (root / 'committed.json').write_text(json.dumps(receipt))
        if mode != 'commit_lost':
            print(json.dumps(receipt))
else:
    raise SystemExit(2)
