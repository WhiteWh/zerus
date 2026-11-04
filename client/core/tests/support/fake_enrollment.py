#!/usr/bin/env python3
"""Workspace protocol fixture. No service calls, credential files or raw logs."""
import json,sys
from pathlib import Path
root=Path(__file__).parent;args=sys.argv[1:]
if args==['--help']:
 print('Workspace stop ABI: service-preflight-v1' if not (root/'old-connector').exists() else 'Older connector');sys.exit()
assert len(args)==4 and args[0]=='--workspace' and args[2]=='--config';assert Path(args[3])==root/'connector.json'
action=args[1];body=json.load(sys.stdin);state_path=root/'status.json';state=json.loads(state_path.read_text());mode=(root/'mode').read_text() if (root/'mode').exists() else ''
def reply(value):print(json.dumps(value));sys.exit()
with (root/'calls.ndjson').open('a') as out:out.write(json.dumps({'action':action,'keys':sorted(body),'expected_revision':body.get('expected_revision'),'generation':body.get('generation')})+'\n')
if action=='status':assert body=={};reply(state)
assert action in ['start','stop','create','join','switch-create','switch-join','check'];assert body['expected_revision']==state['config_revision']
with (root/'effects.ndjson').open('a') as out:out.write(json.dumps({'action':action})+'\n')
if action=='start':
 state.update(running=True,connector_state='running',generation='f'*32);state_path.write_text(json.dumps(state))
 if mode=='unknown':sys.exit(7)
 reply({'state':'running','running':True,'generation':state['generation']})
if action=='stop':
 assert body['generation']==state['generation']
 if mode=='draining':state['connector_state']='stopping';state_path.write_text(json.dumps(state));reply({'state':'stopping','running':True,'generation':state['generation']})
 state.update(running=mode=='replacement',connector_state='running' if mode=='replacement' else 'stopped',generation='c'*32 if mode=='replacement' else None);state_path.write_text(json.dumps(state))
 if mode=='unknown':sys.exit(7)
 reply({'state':'stopped','running':False,'generation':None})
if action.endswith('join'):assert body['code'].startswith('ze1.') and len(body['code'])<=4096
if mode=='unknown':sys.exit(7)
if action=='check' and state.get('pending',{}).get('state')=='legacy-retirement-unconfirmed':
 if not body.get('allow_unconfirmed_legacy_retirement'):reply({'state':'legacy-retirement-unconfirmed','config_revision':state['config_revision'],'node_id':state['pending']['node_id']})
if mode in ['pending','migration-draining','migration-blocked']:
 pending_state='pending' if mode=='pending' else mode;state['pending']={'state':pending_state,'action':state.get('pending',{}).get('action',action),'node_id':'node-new'}
 result={'state':pending_state,'config_revision':state['config_revision'],'node_id':'node-new'}
 if mode.startswith('migration-'):
  phase=mode.removeprefix('migration-');migration={'phase':phase,'retryable':phase=='draining','reason':'draining' if phase=='draining' else 'unsafe-unit'};state['migration']=migration;result.update(encrypted=False,migration=migration)
 state_path.write_text(json.dumps(state));reply(result)
server=body.get('server_url','https://new-relay.example.invalid')
if mode=='wrong-relay':server='https://other.example.invalid'
state={'state':'active','encrypted':True,'config_revision':'b'*64,'server_url':server,'node_id':'node-new','workspace_id':'ws-new','workspace_name':'Workspace','computer_name':'workstation','running':True,'connector_state':'running','generation':'d'*32};state_path.write_text(json.dumps(state));reply(state)
