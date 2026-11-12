#!/usr/bin/env python3
"""Measure payload-sized allocations in the real private CLI metadata lookup."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SIZE = 160003  # Distinct from the bounded raw-file buffer's growth capacities.
SHIM = r'''
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
extern void *__libc_malloc(size_t);
extern void *__libc_realloc(void *, size_t);
static _Atomic size_t hits;
void *malloc(size_t n) {
    if (n == 160003) atomic_fetch_add(&hits, 1);
    return __libc_malloc(n);
}
void *realloc(void *p, size_t n) {
    if (n == 160003) atomic_fetch_add(&hits, 1);
    return __libc_realloc(p, n);
}
__attribute__((destructor)) static void report(void) {
    const char *path = getenv("RECEIPT_ALLOCATION_LOG");
    if (!path) return;
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) return;
    char bytes[64];
    int len = snprintf(bytes, sizeof(bytes), "%zu\n", atomic_load(&hits));
    if (len > 0 && (size_t)len < sizeof(bytes)) { (void)write(fd, bytes, (size_t)len); }
    close(fd);
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'linux' or platform.libc_ver()[0] != 'glibc' or not shutil.which('cc'):
        raise RuntimeError('This allocation control requires Linux/glibc and cc; a skip is not evidence')
    args.output = args.output.resolve(); args.output.mkdir(parents=True, exist_ok=True)
    baseline = args.baseline.resolve(strict=True); candidate = args.candidate.resolve(strict=True)
    sys.path.insert(0, str(ROOT / 'tests'))
    from test_delivery_receipt import DeliveryReceipt, NAME
    rows = []
    with tempfile.TemporaryDirectory(prefix='receipt-alloc-') as scratch:
        scratch = Path(scratch); source = scratch / 'alloc.c'; library = scratch / 'alloc.so'
        source.write_text(SHIM)
        subprocess.run(['cc', '-shared', '-fPIC', '-O2', '-std=c11', '-Wall', '-Wextra', '-Werror', str(source), '-o', str(library)], check=True)
        for provider in ('codex', 'dsh', 'opencode'):
            for field in ('text', 'submitted_text', 'attachments'):
                fixture = DeliveryReceipt(); fixture.setUp()
                try:
                    body = 'x' * SIZE
                    payload = body if field != 'attachments' else [dict(name='fixture.txt', mime='text/plain', data_base64=body)]
                    path = fixture.write(provider, **{field: payload})
                    assert path.stat().st_size < 256 * 1024
                    before = fixture.snapshot()
                    results = {}
                    for label, binary in (('baseline', baseline), ('candidate', candidate)):
                        log = args.output / f'{provider}-{field}-{label}.allocations'
                        env = dict(fixture.env, LD_PRELOAD=str(library), RECEIPT_ALLOCATION_LOG=str(log))
                        run = subprocess.run([str(binary), 'delivery', 'receipt', NAME, '--json'],
                                             input=json.dumps({**fixture.query, 'provider': provider}),
                                             env=env, cwd=fixture.root, capture_output=True, text=True, timeout=10)
                        assert run.returncode == 0, run.stdout + run.stderr
                        proof = json.loads(run.stdout)
                        assert proof['status'] == 'submitted', proof
                        assert 'text' not in proof and 'attachments' not in proof and 'x' * 100 not in run.stdout
                        assert fixture.snapshot() == before, 'lookup changed original evidence'
                        results[label] = int(log.read_text())
                    assert results['baseline'] > 0, ('counter did not detect old payload materialization', provider, field, results)
                    assert results['candidate'] == 0, ('metadata reader allocated a payload-sized value', provider, field, results)
                    rows.append(dict(provider=provider, field=field, payload_bytes=SIZE, allocations=results,
                                     original_bytes_and_mtimes_unchanged=True))
                finally:
                    fixture.doCleanups()
    result = dict(baseline_sha256=hashlib.sha256(baseline.read_bytes()).hexdigest(),
                  candidate_sha256=hashlib.sha256(candidate.read_bytes()).hexdigest(),
                  shim_sha256=hashlib.sha256(SHIM.encode()).hexdigest(), observed_load=os.getloadavg(), rows=rows)
    (args.output / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
