#!/usr/bin/env python3
"""Private phone fixture. Invitation bytes live only in process memory/stdout."""
import json,secrets,sys,time,urllib.parse
from pathlib import Path
root=Path(__file__).parent;args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as out:out.write(json.dumps(args)+'\n')
if args and args[0].startswith('@'):assert args.pop(0)=='@studio'
if args==['--help']:print('mobile phones|invite|revoke-phone');sys.exit()
state=json.loads((root/'phones.json').read_text())
if args==['mobile','phones','--json']:print(json.dumps(state));sys.exit()
assert args[:2] in [['mobile','invite'],['mobile','revoke-phone']]
assert args[args.index('--workspace')+1]==state['workspace_id'];assert args[args.index('--server')+1]==state['server_url'];assert args[-1]=='--json'
mode=(root/'mode').read_text() if (root/'mode').exists() else ''
if args[1]=='invite':
 if mode=='unknown':print('Unconfirmed mobile action',file=sys.stderr);sys.exit(7)
 code=secrets.token_urlsafe(40)
 value={'pair_code':code,'pair_url':'zerus://pair?server='+urllib.parse.quote(state['server_url'],safe='')+'&code='+urllib.parse.quote(code,safe=''),'expires_at':time.time()+600,'workspace_id':state['workspace_id'],'workspace_name':state['workspace_name'],'server_url':state['server_url'],'encrypted':True}
 if mode=='wrong':value['workspace_id']='wrong-workspace'
 print(json.dumps(value));sys.exit()
phone=args[2];state['devices']=[p for p in state['devices'] if p['id']!=phone];(root/'phones.json').write_text(json.dumps(state))
if mode=='unknown':print('Unconfirmed mobile action',file=sys.stderr);sys.exit(7)
print(json.dumps({'revoked':True,'device_id':phone}))
