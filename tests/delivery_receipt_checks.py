"""Shared assertions for an actual native send followed by read-only evidence."""
import json
from pathlib import Path
import subprocess
import uuid


def check_delivery_lookup(case, hgs, env, binding, receipt, provider):
    case.assertIn('delivery_binding', receipt)
    case.assertEqual(receipt['delivery_binding']['session_id'], binding['session_id'])
    query = dict(request_id=str(uuid.uuid4()), message_request_id=receipt['request_id'],
                 expected_machine_id=receipt['delivery_binding']['machine_id'], expected_session_id=binding['session_id'],
                 expected_run_id=binding['run_id'], expected_conversation_id=binding['conversation_id'],
                 provider=provider, native_digest=receipt['digest'])
    directory = {'dsh': 'dsh/receipts', 'opencode': 'opencode/receipts'}[provider]
    original = Path(env['HGS_STATE_DIR']) / directory / (receipt['request_id'] + '.json')
    before = original.read_bytes(), original.stat().st_mtime_ns
    lookup_env = {**env, 'HGS_STATE_DIR': str(Path(env['HGS_STATE_DIR']).resolve()),
                  'HGS_CONFIG_DIR': str(Path(env['HGS_CONFIG_DIR']).resolve())}
    for _ in range(2):
        result = subprocess.run([str(hgs), 'delivery', 'receipt', binding['name'], '--json'],
                                input=json.dumps(query), env=lookup_env, capture_output=True, text=True, timeout=10)
        case.assertEqual(result.returncode, 0, result.stderr)
        proof = json.loads(result.stdout)
        case.assertEqual(proof['status'], 'submitted', proof)
        case.assertEqual(proof['message_request_id'], receipt['request_id'])
        case.assertEqual(proof['native_digest'], receipt['digest'])
    case.assertEqual((original.read_bytes(), original.stat().st_mtime_ns), before)
    print('DELIVERY_LOOKUP_CONTROL=' + json.dumps(dict(provider=provider, native_version=getattr(case, 'version', None),
          exact_original_binding=True, original_bytes_and_mtime_unchanged=True, lookups=2)), flush=True)
