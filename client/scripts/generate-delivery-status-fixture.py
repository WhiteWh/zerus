#!/usr/bin/env python3
"""Extract client test inputs from the real root status producer, never ready flags.

cargo test --locked desktop::delivery::tests::actual_root_projection_is_consumed_by_the_client_without_ready_json_fixtures -- --exact --nocapture |
    python3 client/scripts/generate-delivery-status-fixture.py [--check]
"""
import argparse
import json
from pathlib import Path
import re
import sys
import uuid

FIELDS = ('abi', 'configuration', 'epoch', 'generation', 'lifecycle', 'automatic_approvals', 'catalog_reads')
LIFECYCLE_FIELDS = ('abi', 'configuration', 'epoch', 'generation', 'ipc_ready', 'phase', 'background')
CASES = {'awaiting_handoff', 'disabled_but_ready', 'enabled', 'approval_degraded',
         'disabled_while_degraded', 'read_degraded', 'drained'}
OUTPUT = Path(__file__).resolve().parents[1] / 'fixtures/desktop-delivery-status.json'


def inputs(text):
    if not text.lstrip().startswith('['):
        marker = 'ROOT_DELIVERY_STATUS '
        if marker not in text:
            raise ValueError('missing real root producer output')
        text = text.split(marker, 1)[1]
    rows, _ = json.JSONDecoder().raw_decode(text.lstrip())
    if not isinstance(rows, list) or len(rows) != len(CASES) or {row['case'] for row in rows} != CASES:
        raise ValueError('unexpected root producer cases')
    result = []
    for row in rows:
        status = row['status']
        if status['abi'] != 'desktop-v1' or not re.fullmatch('[0-9a-f]{64}', status['configuration']):
            raise ValueError('invalid producer scope')
        if str(uuid.UUID(status['epoch'])) != status['epoch']:
            raise ValueError('invalid producer epoch')
        lifecycle = status['lifecycle']
        for key in ('configuration', 'epoch', 'generation'):
            if lifecycle[key] != status[key]:
                raise ValueError('inconsistent producer scope')
        selected = {key: status[key] for key in FIELDS}
        selected['lifecycle'] = {key: lifecycle[key] for key in LIFECYCLE_FIELDS}
        # Stable synthetic identities only. Every readiness/policy value comes
        # unchanged from the production projection/decorator above.
        for value in (selected, selected['lifecycle']):
            value['configuration'] = 'a' * 64
            value['epoch'] = '00000000-0000-4000-8000-000000000001'
        result.append({'case': row['case'], 'status': selected})
    return result


def contract(rows):
    # Private fixture Store paths randomize these opaque IDs. Readiness, policy
    # and all other values still compare exactly to the production decorator.
    rows = json.loads(json.dumps(rows))
    for row in rows:
        for value in (row['status'], row['status']['lifecycle']):
            value['configuration'] = '<configuration>'
            value['epoch'] = '<epoch>'
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    raw = sys.stdin.read((1 << 20) + 1)
    if len(raw) > 1 << 20:
        raise ValueError('root fixture output exceeds its limit')
    rows = inputs(raw)
    if args.check:
        if contract(rows) != contract(json.loads(OUTPUT.read_text())):
            raise ValueError('client fixture differs from the actual root producer')
    else:
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        OUTPUT.write_text(json.dumps(rows, indent=2, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
