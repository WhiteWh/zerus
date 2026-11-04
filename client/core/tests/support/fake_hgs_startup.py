#!/usr/bin/env python3
"""Private startup protocol adversary; never executes a real hgs command."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(os.environ['TEST_STARTUP_ROOT'])
mode = (root / 'mode').read_text()
with (root / 'calls').open('a') as out:
    out.write(json.dumps(sys.argv[1:]) + '\n')
if sys.argv[1:] == ['--help']:
    if mode == 'bad-utf8':
        sys.stdout.buffer.write(b'Desktop startup ABI: desktop-startup-v1\n\xff\n')
        sys.stdout.buffer.flush()
        sys.exit(0)
    print('Desktop startup ABI: desktop-startup-v1' if mode != 'old' else 'Desktop service ABI: desktop-v1')
    sys.exit(0)
if sys.argv[1:] != ['desktop', 'startup', '--json']:
    sys.exit(9)
(root / 'pid').write_text(str(os.getpid()))
hold = json.loads(sys.stdin.readline())
ready = dict(hold, op='ready', configuration='a' * 64)
if mode == 'nonce':
    ready['nonce'] = '22222222-2222-4222-8222-222222222222'
if mode == 'extra':
    ready['authority'] = True
if mode == 'oversize':
    print('x' * 4097, flush=True)
    sys.exit(0)
if mode == 'partial':
    print('{', end='', flush=True)
    sys.exit(0)
if mode == 'stall':
    time.sleep(2)
    sys.exit(0)
print(json.dumps(ready), flush=True)
if mode == 'duplicate':
    print(json.dumps(ready), flush=True)
line = sys.stdin.readline()
if not line:
    sys.exit(4)
release = json.loads(line)
assert release['op'] == 'release'
assert release['nonce'] == hold['nonce']
assert release['configuration'] == ready['configuration']
if mode in ('held', 'unknown'):
    delay = 0.25 if mode == 'held' else 2.5
    child = subprocess.Popen([sys.executable, '-c',
                              'import pathlib,sys,time;time.sleep(float(sys.argv[1]));pathlib.Path(sys.argv[2]).write_text("ended")',
                              str(delay), str(root / 'reader-ended')], stdin=subprocess.DEVNULL)
    (root / 'descendant').write_text(str(child.pid))
print(json.dumps(dict(release, op='released')), flush=True)
if mode == 'tail':
    print('{}', flush=True)
sys.exit(3 if mode == 'failure' else 0)
