#!/usr/bin/env python3
"""Computer-code fixture; secret bytes never reach files or arguments."""
import json,os,secrets,sys,time
from pathlib import Path
root=Path(__file__).parent
args=sys.argv[1:]
assert len(args)==4 and args[0]=='--workspace' and args[2]=='--config'
assert Path(args[3])==root/'connector.json'
payload=json.load(sys.stdin)
assert all(k not in os.environ for k in ['HGS_SESSION','HGS_RUN_ID','HGS_EXPECTED_ID','HGS_AGENT','HGS_HOST_PANE','HGS_HOST_SESSION','TMUX','TMUX_PANE'])
state=json.loads((root/'status.json').read_text())
mode=(root/'mode').read_text() if (root/'mode').exists() else ''
action=args[1]
assert action in ['status','invite-computer','cancel-invite','check']
with (root/'calls.ndjson').open('a') as out:out.write(json.dumps({'action':action,'args':args,'payload':payload})+'\n')
if action=='status':assert payload=={};print(json.dumps(state));sys.exit()
assert set(payload)==({'expected_revision','invitation_id'} if action=='cancel-invite' else {'expected_revision'})
if mode=='raced' or payload['expected_revision']!=state['config_revision']:sys.exit(1)
with (root/'effects.ndjson').open('a') as out:out.write(json.dumps({'action':action})+'\n')
if mode=='unknown':print('Unconfirmed workspace action',file=sys.stderr);sys.exit(7)
if action=='invite-computer':
 code='ze1.'+secrets.token_urlsafe(60)
 print(json.dumps({'purpose':'phone' if mode=='wrong-purpose' else 'computer','code':code,'server_url':'https://other.example.invalid' if mode=='wrong-relay' else state['server_url'],'invitation_id':'11111111-1111-4111-8111-111111111111','expires_at':time.time()+600}));sys.exit()
if action=='cancel-invite':print(json.dumps({'cancelled':mode!='bad-receipt'}));sys.exit()
print(json.dumps(state))
