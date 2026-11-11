#!/usr/bin/env python3
"""Measure one exact read against private one-receipt and large receipt stores."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hgs', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--foreign', type=int, default=10000)
    args = parser.parse_args()
    if not 10000 <= args.foreign <= 100000:
        parser.error('Use 10,000 to 100,000 private foreign originals')
    trace = shutil.which('strace')
    if not trace:
        raise RuntimeError('strace is required; skipped operation counts are not evidence')
    os.environ['HGS_TEST_BIN'] = str(args.hgs.resolve(strict=True))
    sys.path.insert(0, str(ROOT / 'tests'))
    from test_delivery_receipt import DeliveryReceipt, NAME
    fixtures = [DeliveryReceipt(), DeliveryReceipt()]
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    try:
        for fixture in fixtures:
            fixture.setUp(); fixture.write()
        directory = fixtures[1].state / 'input_receipts'
        template = json.loads(next(directory.glob('*.json')).read_text())
        template['name'] = 'codex/foreign/receipt'
        template['conversation_id'] = str(uuid.uuid4())
        for n in range(1, args.foreign + 1):
            identity = str(uuid.UUID(int=n)); template['request_id'] = identity
            path = directory / (identity + '.json')
            path.write_text(json.dumps(template)); path.chmod(0o600)
        before = [fixture.snapshot() for fixture in fixtures]
        counts = []; latencies = [[], []]; loads = []
        for size, fixture in zip((1, args.foreign + 1), fixtures):
            log = args.output / f'originals-{size}.strace'
            result = subprocess.run([trace, '-f', '-yy', '-s', '4096', '-o', str(log), '-e',
                                     'trace=%file,read,getdents64,fcntl,flock,connect',
                                     str(args.hgs.resolve()), 'delivery', 'receipt', NAME, '--json'],
                                    input=json.dumps(fixture.query), env=fixture.env, cwd=fixture.root,
                                    capture_output=True, text=True, timeout=10)
            # Use the same literal original name as the retained exact query.
            if result.returncode:
                raise AssertionError(result.stdout + result.stderr)
            proof = json.loads(result.stdout)
            assert proof['status'] == 'submitted', proof
            lines = log.read_text().splitlines(); source = str(fixture.state / 'input_receipts')
            native = [line for line in lines if source in line]
            opens = [line for line in native if any(call in line for call in ('open(', 'openat(', 'openat2(')) and '.json' in line]
            enumeration = [line for line in native if 'getdents' in line]
            writes = [line for line in native if any(flag in line for flag in ('O_WRONLY', 'O_RDWR', 'O_CREAT'))]
            locks = [line for line in native if any(flag in line for flag in ('F_SETLK', 'F_OFD_SETLK', 'flock('))]
            own_opens = [line for line in lines if str(fixture.root) in line and any(call in line for call in ('open(', 'openat(', 'openat2('))]
            own_writes = [line for line in own_opens if any(flag in line for flag in ('O_WRONLY', 'O_RDWR', 'O_CREAT', 'O_TRUNC'))]
            connections = [line for line in lines if 'connect(' in line]
            assert not own_writes and not connections, (own_writes, connections)
            reads = [line for line in native if 'read(' in line]
            stats = [line for line in native if any(call in line for call in ('stat(', 'stat64(', 'newfstatat(', 'statx(')) and '.json' in line]
            assert len(opens) == 1 and not enumeration and not writes and not locks, (opens, enumeration, writes, locks)
            counts.append(dict(originals=size, receipt_opens=len(opens), receipt_reads=len(reads), receipt_stats=len(stats),
                               receipt_directory_reads=len(enumeration), receipt_write_opens=len(writes), receipt_locks=len(locks),
                               fixture_write_opens=len(own_writes), connections=len(connections)))
        for _ in range(12):
            for index, fixture in enumerate(fixtures):
                start = time.monotonic(); proof = fixture.call(); latencies[index].append(time.monotonic() - start)
                assert proof['status'] == 'submitted'
            loads.append(Path('/proc/loadavg').read_text().strip())
        assert all(fixture.snapshot() == old for fixture, old in zip(fixtures, before)), 'lookup changed private evidence'
        result = dict(source=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                      hgs_sha256=hashlib.sha256(args.hgs.read_bytes()).hexdigest(), counts=counts,
                      latency_seconds=latencies, medians_seconds=list(map(statistics.median, latencies)),
                      observed_loads=loads, original_bytes_and_mtimes_unchanged=True)
        (args.output / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2))
    finally:
        for fixture in fixtures: fixture.doCleanups()


if __name__ == '__main__':
    main()
