#!/usr/bin/env python3
"""Private catalog-only transfer fixture. Contains no credential bytes."""
import json,sys
from pathlib import Path
root=Path(__file__).parent;args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as log:log.write(json.dumps(args)+'\n')
if args==['--help']:
    print('Account transfer ABI: revision-copy-v1' if not (root/'legacy').exists() else 'legacy hgs')
    sys.exit()
if args[0].startswith('@'):
    host=args.pop(0)[1:];expected={'studio':'11111111-1111-4111-8111-111111111111','lab':'22222222-2222-4222-8222-222222222222'}[host]
    assert args[:2]==['--expect-machine',expected];args=args[2:]
    assert args==['account','ls']
    print((root/(host+'.json')).read_text());sys.exit()
assert args[:2]==['account','copy']
get=lambda key:args[args.index(key)+1]
source=json.loads((root/'studio.json').read_text());target=json.loads((root/'lab.json').read_text())
assert get('--source-machine')=='11111111-1111-4111-8111-111111111111'
assert get('--destination-machine')=='22222222-2222-4222-8222-222222222222'
assert get('--source-revision')==source['revision'] and get('--destination-revision')==target['revision']
assert get('--source-auth-revision')==source['profiles'][0]['auth_revision']
assert get('--from')=='studio' and get('--to')=='lab'
profile={'id':get('--as'),'provider':'codex','label':get('--label'),'home':'/accounts/copied','auth_revision':'copied-auth'}
target['profiles'].append(profile);target['revision']='r2';(root/'lab.json').write_text(json.dumps(target))
if (root/'uncertain').exists():print('Synthetic uncertain copy',file=sys.stderr);sys.exit(7)
print(json.dumps({'ok':True,'id':profile['id'],'provider':'codex','host':'lab'}))
