#!/usr/bin/env python3
"""Real hgs send -> lost connector result -> restart -> exact READ, privately."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hgs', type=Path, required=True)
    parser.add_argument('--test-binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--old-hgs', type=Path)
    args = parser.parse_args()
    hgs = args.hgs.resolve(strict=True)
    binary = args.test_binary.resolve(strict=True)
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=True)
    os.environ['HGS_TEST_BIN'] = str(hgs)
    sys.path.insert(0, str(ROOT / 'tests'))
    from test_input import InputTransport
    results = []
    if args.old_hgs:
        from test_delivery_receipt import DeliveryReceipt, NAME
        legacy = DeliveryReceipt(); legacy.setUp()
        try:
            for source in ('absent', 'present'):
                if source == 'present': legacy.write()
                before = legacy.snapshot()
                result = subprocess.run([str(args.old_hgs.resolve(strict=True)), '__state', 'delivery', 'receipt', NAME, '--json'],
                                        input=json.dumps(legacy.query), env=legacy.env, cwd=legacy.root,
                                        capture_output=True, text=True, timeout=10)
                (output / ('old-cli-' + source + '.stdout')).write_text(result.stdout)
                (output / ('old-cli-' + source + '.stderr')).write_text(result.stderr)
                assert result.returncode != 0, 'old CLI must refuse the closed operation'
                assert legacy.snapshot() == before, 'downgrade changed private receipt or state evidence'
                if source == 'absent': assert not legacy.state.exists()
                results.append(dict(mode='downgrade-' + source, refused=True, original_bytes_and_mtimes_unchanged=True))
        finally:
            legacy.doCleanups()
    for mode in ('direct', 'routed', 'encrypted'):
        fixture = InputTransport(methodName='runTest')
        fixture.setUp()
        try:
            # An explicit small environment has no calling-agent identity, API
            # keys or provider home. The only agent is the owned scripted pane.
            keep = ('PATH', 'HOME', 'INPUT_FIXTURE', 'HGS_CONFIG_DIR', 'HGS_STATE_DIR', 'HGS_SELF', 'HGS_TAB')
            env = {k: fixture.env[k] for k in keep}; env.update(HGS_PEERS='', TMPDIR=str(fixture.root))
            for key in ('CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'KIMI_CODE_HOME', 'DSH_HOME', 'OPENCODE_HOME',
                        'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME'):
                private = fixture.root / key.lower(); private.mkdir(mode=0o700); env[key] = str(private)
            fixture.env = env
            session = str(uuid.uuid4()); machine = str(uuid.uuid4())
            fixture.record['session_id'] = session; fixture.write_record()
            config = fixture.root / 'config'; (config / 'swarm').mkdir(mode=0o700, parents=True)
            config.chmod(0o700)
            catalog = config / 'swarm/catalog.json'
            catalog.write_text(json.dumps(dict(node_id=machine, catalog=dict(schema=1, swarm_id=str(uuid.uuid4()), operations={}), bindings={})))
            catalog.chmod(0o600)
            original = fixture.payload(text='  cafe\u0301\nexact whitespace  ')
            intent = dict(schema=1, computer_id='fixture', machine_id=machine, session_id=session,
                          session=fixture.name, provider='codex', native_encoding='compact' if mode == 'routed' else 'python_default',
                          payload=original)
            data = dict(hgs=str(hgs), env=env, mode=mode, intent=intent, journal=str(fixture.root / 'connector'),
                        original=str(fixture.state / 'input_receipts' / (original['request_id'] + '.json')))
            path = fixture.root / 'fixture.json'; path.write_text(json.dumps(data)); path.chmod(0o600)
            result = subprocess.run([str(binary), 'actual_native_submission_lost_result_restart_and_read_never_replays',
                                     '--exact', '--ignored', '--nocapture', '--test-threads=1'],
                                    env={**env, 'ZERUS_SEND_RECEIPT_FIXTURE': str(path)}, cwd=fixture.root,
                                    capture_output=True, text=True, timeout=60)
            (output / (mode + '.stdout')).write_text(result.stdout)
            (output / (mode + '.stderr')).write_text(result.stderr)
            assert result.returncode == 0, result.stdout + result.stderr
            fixture.wait_for(lambda: fixture.received().endswith(b'\r'))
            receipt = json.loads(Path(data['original']).read_text())
            # The unchanged terminal producer trims the pasted body, while its
            # digest binds the exact original stdin, including outer whitespace.
            assert receipt['submitted_text'] == original['text'].strip()
            expected = b'\x1b[200~' + receipt['submitted_text'].encode() + b'\x1b[201~\r'
            assert fixture.received() == expected, ('one exact paste and Enter required', fixture.received())
            results.append(dict(mode=mode, actual_send_count=1, lookups=2, original_journal_unchanged=True))
        finally:
            fixture.tearDown(); fixture.doCleanups()
    receipt = dict(source=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                   hgs_sha256=hashlib.sha256(hgs.read_bytes()).hexdigest(),
                   test_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), results=results)
    (output / 'results.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
