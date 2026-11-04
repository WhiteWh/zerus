#!/usr/bin/env python3
"""Private clone adapter fixture. No network, Git or checkout creation."""
import json
import os
import sys
import time
from pathlib import Path
here = Path(os.path.abspath(__file__)).parent
config = json.loads((here / 'config.json').read_text())
args = sys.argv[1:]
with (here / 'calls.jsonl').open('a') as f:
    f.write(json.dumps(args) + '\n')
assert args[:2] == ['--expect-machine', config['machine']]
args = args[2:]
if '--help' in args:
    print('repository-clone-v1' if config.get('supported', True) else 'usage')
    sys.exit(0)
assert args[:1] == ['repository']
operation = args[1]
if operation == 'check':
    if config.get('gate'):
        (here / 'checking').touch()
        until = time.monotonic() + 8
        while not (here / 'release').exists() and time.monotonic() < until:
            time.sleep(0.02)
    parent, name = args[args.index('--parent') + 1], args[args.index('--name') + 1]
    print(json.dumps(dict(status='ready', parent=parent, name=name, path=parent + '/' + name)))
else:
    request = args[args.index('--request-id') + 1]
    saved = json.loads((here / 'settings.json').read_text())
    records = saved['repository_clones']['entries'].values()
    record = next(v['pending'] for v in records if v.get('pending', {}).get('request_id') == request)
    print(json.dumps(dict(record, status=config.get('result', 'cloned'))))
