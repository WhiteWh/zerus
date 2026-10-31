#!/usr/bin/env python3
"""Native account isolation/transport integration checks. Uses only temporary credentials."""
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

HGS = str(Path(os.environ.get('HGS_TEST_BIN', 'target/debug/hgs')).resolve())

class Accounts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zerus-accounts-test-')
        self.root = Path(self.temp.name)
        self.home = self.root / 'home'; self.home.mkdir()
        self.config = self.home / '.config/hgs'; self.config.mkdir(parents=True)
        self.env = {k:v for k,v in os.environ.items() if not k.startswith('HGS_') and k not in ('CODEX_HOME','CLAUDE_CONFIG_DIR','KIMI_CODE_HOME','TMUX','TMUX_PANE')}
        self.env.update(HOME=str(self.home), HGS_CONFIG_DIR=str(self.config), HGS_STATE_DIR=str(self.root/'state'), HGS_SELF='test', HGS_PEERS='', HGS_TAB='0')
        # Private XDG directories: nothing reaches the real OpenCode state or config.
        for name,folder in (('XDG_STATE_HOME','xdg-state'),('XDG_CONFIG_HOME','xdg-config'),('XDG_DATA_HOME','xdg-data'),('XDG_CACHE_HOME','xdg-cache')):
            (self.root/folder).mkdir(); self.env[name]=str(self.root/folder)
        # HGS prepends HOME/.local/bin, /opt/homebrew/bin and /usr/local/bin.
        # Put mocks in the first of those, so Homebrew can never win on macOS.
        self.bin = self.home/'.local/bin'; self.bin.mkdir(parents=True)
        self.opencode_calls = self.root/'opencode-calls.log'
        for executable in ('codex','claude','kimi','dsh','ssh','security','tmux','opencode'):
            script = self.bin/executable
            if executable == 'opencode':
                # Logs every argument list and answers only `--version`, so a service
                # start or any other command shows. A test that needs more installs its own fake.
                script.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "'+str(self.opencode_calls)+'"\n'
                                  '[ "$#" -eq 1 ] && [ "$1" = --version ] && echo v2.0.32 && exit 0\nexit 9\n')
            else:
                script.write_text('#!/bin/sh\nexit '+('1' if executable == 'tmux' else '99')+'\n')
            script.chmod(0o755)
        self.env['PATH'] = str(self.bin) + os.pathsep + self.env['PATH']
        prepared_path = os.pathsep.join((str(self.home/'.local/bin'), '/opt/homebrew/bin', '/usr/local/bin', self.env['PATH']))
        for executable in ('codex','claude','kimi','dsh','ssh','security','tmux','opencode'):
            resolved = shutil.which(executable, path=prepared_path)
            self.assertIsNotNone(resolved)
            self.assertEqual(Path(resolved).resolve(), (self.bin/executable).resolve(),
                             'Test isolation failed: HGS could invoke a real '+executable)
    def tearDown(self): self.temp.cleanup()
    def run_hgs(self,*args,ok=True,input=None):
        p = subprocess.run([HGS,*args],env=self.env,cwd=self.root,input=input,capture_output=True,timeout=15)
        self.assertEqual(p.returncode == 0,ok,(args,p.stderr.decode()))
        return p
    def ls(self): return json.loads(self.run_hgs('account','ls').stdout)
    def add(self,id='work',provider='codex'):
        return json.loads(self.run_hgs('account','add',id,'--provider',provider,'--label','Work').stdout)
    def fake_auth(self):
        d=self.home/'.codex'; d.mkdir(); (d/'auth.json').write_text(json.dumps({'OPENAI_API_KEY':'TEMPORARY-TEST-KEY'})); return d
    def assert_opencode_only_asked_its_version(self):
        """Nothing so far ran OpenCode except `--version`. `account ls` then asks for
        the version, which proves the logging fake is the `opencode` hgs runs."""
        calls=self.opencode_calls.read_text().splitlines() if self.opencode_calls.exists() else []
        self.assertEqual([c for c in calls if c!='--version'],[],'OpenCode was asked more than its version')
        self.ls()
        self.assertIn('--version',self.opencode_calls.read_text().splitlines())
        self.assertFalse((self.root/'xdg-state/opencode').exists(),'a service registration appeared')
    def test_opencode_full_catalog_never_starts_and_checks_its_folder(self):
        catalog=json.loads(self.run_hgs('account','models','native-opencode','--json').stdout)
        self.assertEqual((catalog['catalog_state'],catalog['models'],catalog['model_options']),('not_running',[],[]))
        self.assertEqual(catalog['location'],os.path.realpath(self.home))
        folder=self.root/'project';folder.mkdir()
        located=json.loads(self.run_hgs('account','models','native-opencode','--location',str(folder),'--json').stdout)
        self.assertEqual((located['location'],located['providers']),(os.path.realpath(folder),[]))
        for location in (('relative/folder',),(str(self.root/'missing'),),()):
            # A --location without a folder is refused, never read as the home folder.
            refused=self.run_hgs('account','models','native-opencode','--location',*location,ok=False)
            self.assertIn(b'Choose an existing folder on this computer',refused.stderr)
        refused=self.run_hgs('account','models','native-codex',ok=False)
        self.assertIn(b'Only the native OpenCode account lists OpenCode models',refused.stderr)
        self.assert_opencode_only_asked_its_version()
    def test_stopped_opencode_session_details_carry_the_cached_short_list(self):
        name='opencode/workspace/stopped'
        record={'backend':'opencode','agent':'opencode','name':name,'cwd':str(self.home),'run_id':'7b0c6f1e-2d4a-4c8e-9f10-1a2b3c4d5e6f',
                'conversation_id':'ses_'+'A'*26,'created':1792267200.0,'launch_id':None,'service':'opencode2'}
        binding=self.root/'state/opencode/bindings'/(hashlib.sha256(name.encode()).hexdigest()+'.json')
        binding.parent.mkdir(parents=True);binding.write_text(json.dumps(record))
        candidates=[{'value':'fixture/local','provider':'fixture','label':'Native fixture','provider_label':'Private keyless fixture',
                     'free':False,'effort_options':['low','high'],'configured':True},
                    {'value':'opencode/big-pickle','provider':'opencode','label':'Big Pickle','provider_label':'OpenCode',
                     'free':True,'effort_options':[],'configured':False}]
        cache={'version':2,'location':str(self.home),'model_options':[],'candidates':candidates,'models_truncated':False,
               'providers':[],'providers_truncated':False,'settings_model':'fixture/local','catalog_at':1792267200.5}
        (self.root/'state/opencode/catalog.json').write_text(json.dumps(cache))
        details=json.loads(self.run_hgs('inspect',name).stdout)
        self.assertEqual((details['name'],details['inspection_unavailable']),(name,True))
        self.assertIn('OpenCode is not running',details['inspection_reason'])
        # Clients take details without model_options_truncated for an older hgs.
        self.assertIs(details['model_options_truncated'],False)
        options=details['model_options']
        self.assertEqual([o['value'] for o in options],['fixture/local','opencode/big-pickle'])
        self.assertEqual([o.get('default') for o in options],[True,None])
        self.assert_opencode_only_asked_its_version()
    def test_model_defaults_are_account_scoped_catalog_validated_and_revision_checked(self):
        self.add('code','codex');self.add('second','codex');self.add('moon','kimi')
        home=self.config/'accounts/code'
        (home/'models_cache.json').write_text(json.dumps({'models':[{'slug':'fixture-smart','display_name':'Fixture smart',
            'visibility':'list','supported_reasoning_levels':[{'effort':'low'},{'effort':'xhigh'}],'default_reasoning_level':'low'}]}))
        (home/'config.toml').write_text('model="fixture-smart"\nmodel_reasoning_effort="low"\nprivate_token="TEMPORARY-PRIVATE-KEY"\n')
        original=(home/'config.toml').read_bytes()
        settings=json.loads(self.run_hgs('account','model-settings','code').stdout)
        self.assertEqual(settings['provider_defaults'],dict(model='fixture-smart',effort='low'))
        self.assertEqual(settings['models'][0]['effort_options'],['low','xhigh'])
        self.assertNotIn('TEMPORARY',json.dumps(settings))
        saved=json.loads(self.run_hgs('account','model-defaults','code','--model','fixture-smart','--effort','xhigh','--revision',settings['revision']).stdout)
        profiles={p['id']:p for p in saved['profiles']}
        self.assertEqual(profiles['code']['model_defaults'],dict(model='fixture-smart',effort='xhigh'))
        self.assertEqual(profiles['second']['model_defaults'],dict(model='',effort=''))
        self.run_hgs('account','model-defaults','code','--model','','--revision',settings['revision'],ok=False)
        self.run_hgs('account','model-defaults','code','--model','foreign','--effort','low',ok=False)
        self.run_hgs('account','model-defaults','code','--model','fixture-smart','--effort','ultra',ok=False)
        self.run_hgs('account','model-defaults','code','--model','','--dry-run')
        self.assertEqual((home/'config.toml').read_bytes(),original)
        self.assertEqual(json.loads(self.run_hgs('account','model-settings','code').stdout)['defaults']['effort'],'xhigh')
        moon=self.config/'accounts/moon'
        (moon/'config.toml').write_text('[models.fixture]\nprovider="fixture"\nmodel="fixture"\ncapabilities=["thinking"]\n')
        self.run_hgs('account','model-defaults','moon','--model','fixture','--effort','on',ok=False)
        self.run_hgs('account','model-defaults','moon','--model','fixture')
        self.run_hgs('account','model-defaults','code','--model','')
        self.assertEqual(json.loads(self.run_hgs('account','model-settings','code').stdout)['defaults'],dict(model='',effort=''))
        self.assertEqual((home/'config.toml').read_bytes(),original)

    def test_native_opencode_login_dry_run_never_invokes_auth(self):
        fake=self.bin
        binary=fake/"opencode";marker=self.root/"opencode-auth-marker"
        binary.write_text("#!/bin/sh\nprintf called > "+str(marker)+"\n");binary.chmod(0o700)
        previous=self.env.get("PATH","");self.env["PATH"]=str(fake)+":"+previous
        try:
            planned=self.run_hgs('account','login','native-opencode','--dry-run')
            self.assertIn(b'Connect OpenCode providers with `opencode auth login` in a terminal on this computer',planned.stdout)
            self.assertFalse(marker.exists())
            refused=self.run_hgs('account','login','native-opencode',ok=False)
            self.assertIn(b'in a terminal on this computer',refused.stderr)
            self.assertFalse(marker.exists())
        finally:self.env["PATH"]=previous

    def test_native_opencode_row_without_a_service_and_start_scope(self):
        fake=self.bin;binary=fake/"opencode"
        binary.write_text("#!/bin/sh\n[ \"$1\" = --version ] && echo v2.0.32 || exit 9\n");binary.chmod(0o700)
        previous=self.env.get("PATH","");self.env["PATH"]=str(fake)+":"+previous
        try:
            row=[p for p in self.ls()['profiles'] if p['id']=='native-opencode'][0]
            self.assertTrue(row['installed'])
            self.assertEqual(row['catalog_state'],'not_running')
            self.assertEqual(row['model_options'],[])
            self.assertEqual((row['providers'],row['model_options_truncated'],row['providers_truncated']),([],False,False))
            self.assertLessEqual(len(json.dumps(row,ensure_ascii=True)),32*1024)
            binary.write_text("#!/bin/sh\n[ \"$1\" = --version ] && echo 1.4.0 || exit 9\n")
            row=[p for p in self.ls()['profiles'] if p['id']=='native-opencode'][0]
            self.assertFalse(row['installed'])
            self.assertIn('OpenCode 1.4.0 is not supported',row['install_reason'])
            refused=self.run_hgs('account','start','native-codex',ok=False)
            self.assertIn(b'Only the native OpenCode account has a service to start',refused.stderr)
        finally:self.env["PATH"]=previous

    def test_a_damaged_opencode_catalog_cache_keeps_the_stopped_row_small_and_allowlisted(self):
        # Private OpenCode state: an inherited XDG_STATE_HOME must never reach a real service.
        self.env.update(XDG_STATE_HOME=str(self.root/'xdg-state'),XDG_CONFIG_HOME=str(self.root/'xdg-config'))
        binary=self.bin/"opencode";binary.write_text("#!/bin/sh\n[ \"$1\" = --version ] && echo v2.0.32 || exit 9\n");binary.chmod(0o700)
        canary='canary-secret-7f3a';huge='"\\'*1000
        default={'id':'fixture/local','value':'fixture/local','label':huge,'provider':'fixture','provider_label':huge,'free':huge,
                 'effort_options':['e%d'%i for i in range(400)],'effort_labels':[huge]*10,'default':True,'configured':True,'settings':{'apiKey':canary}}
        entries=[default]+[{'id':huge,'value':huge,'label':huge,'provider':huge}]*15+[
            {'value':'keyed/m%03d'%i,'provider':'keyed','label':huge[:300],'provider_label':huge[:300],'free':True,'baseURL':canary} for i in range(250)]
        providers=[{'id':'p%03d'%i,'label':huge,'kind':'key','credential_count':1,'model_count':1,'active_label':huge,'headers':canary} for i in range(15)]
        record={'version':2,'location':str(self.home),'model_options':entries[:3],'candidates':entries,'providers':providers,
                'providers_truncated':{'x':huge},'settings_model':'fixture/local','catalog_at':{'x':huge}}
        cache=self.root/'state/opencode/catalog.json';cache.parent.mkdir(parents=True);cache.write_text(json.dumps(record))
        self.assertTrue(700_000<cache.stat().st_size<1_048_576,cache.stat().st_size)
        row=[p for p in self.ls()['profiles'] if p['id']=='native-opencode'][0]
        self.assertEqual(row['catalog_state'],'not_running')
        self.assertLessEqual(len(json.dumps(row,ensure_ascii=True)),32*1024)
        self.assertFalse(canary in json.dumps(row),'a dropped cache field reached the row')
        self.assertEqual((row['catalog_at'],row['settings_model']),(None,'fixture/local'))
        self.assertEqual((row['model_options_truncated'],row['providers_truncated']),(True,False))
        first=row['model_options'][0]
        self.assertEqual((first['value'],first['default'],first['label'],first['provider_label'],first['free']),('fixture/local',True,'local','fixture',False))
        self.assertEqual(first['effort_options'],['e%d'%i for i in range(16)])
        entry_keys={'id','value','label','provider','provider_label','free','effort_options','effort_labels','favorite','default'}
        for option in row['model_options']:
            self.assertLessEqual(set(option),entry_keys)
            self.assertEqual(option['id'],option['value'])
        self.assertEqual([p['label'] for p in row['providers']],['p%03d'%i for i in range(15)])
        for provider in row['providers']:
            self.assertLessEqual(set(provider),{'id','label','kind','credential_count','model_count','active_label'})
            self.assertNotIn('active_label',provider)

    def test_opencode_favorites_are_stored_beside_the_catalog_with_request_identity(self):
        # Private OpenCode state: an inherited XDG_STATE_HOME must never reach a real service.
        self.env.update(XDG_STATE_HOME=str(self.root/'xdg-state'),XDG_CONFIG_HOME=str(self.root/'xdg-config'))
        catalog=self.config/'accounts.json'
        request='6f1c1a0e-4d8b-4b7e-9a51-3c2d1e0f9a10'
        def favorite(*args,ok=True):
            return self.run_hgs('account','favorite','native-opencode',*args,ok=ok)
        receipt=json.loads(favorite('--model','groq/openai/gpt-oss-20b','--favorite','on','--request-id',request,'--json').stdout)
        self.assertEqual((receipt['status'],receipt['favorite'],receipt['favorites']),('applied',True,['groq/openai/gpt-oss-20b']))
        stored=json.loads((self.config/'opencode-favorites.json').read_text())
        self.assertEqual(stored,{'version':1,'models':['groq/openai/gpt-oss-20b']})
        self.assertFalse(catalog.exists(),'favorites never rewrite accounts.json')
        self.assertEqual(json.loads(favorite('--model','groq/openai/gpt-oss-20b','--favorite','on','--request-id',request).stdout),receipt)
        for args,message in [(('--model','groq/x','--favorite','maybe','--request-id',request),b'Choose --favorite on or off'),
                             (('--model','groq/x','--favorite','on'),b'A favorite change needs --request-id UUID'),
                             (('--favorite','on','--request-id',request),b'Choose --model PROVIDER/MODEL')]:
            self.assertIn(message,favorite(*args,ok=False).stderr)
        refused=self.run_hgs('account','favorite','native-codex','--model','a/b','--favorite','on','--request-id',request,ok=False)
        self.assertIn(b'Only the native OpenCode account has favorite models',refused.stderr)
        planned=json.loads(favorite('--model','fixture/local','--favorite','on','--request-id','6f1c1a0e-4d8b-4b7e-9a51-3c2d1e0f9a11','--dry-run').stdout)
        self.assertEqual(planned,{'dry_run':True,'model':'fixture/local','favorite':True})
        for model,req in (('nonsense','6f1c1a0e-4d8b-4b7e-9a51-3c2d1e0f9a12'),('fixture/local','not-a-uuid')):
            self.assertNotEqual(favorite('--model',model,'--favorite','on','--request-id',req,'--dry-run',ok=False).returncode,0)
        self.assertNotEqual(self.run_hgs('account','favorite','native-codex','--model','a/b','--favorite','on','--request-id',request,'--dry-run',ok=False).returncode,0)
        self.assertEqual(json.loads((self.config/'opencode-favorites.json').read_text())['models'],['groq/openai/gpt-oss-20b'])
        self.assertFalse(catalog.exists(),'favorites never rewrite accounts.json')
        self.assert_opencode_only_asked_its_version()

    def test_defaults_do_not_touch_agent_homes(self):
        data=self.ls(); self.assertEqual({p['provider'] for p in data['profiles'] if p['provider'] not in {'dsh','opencode'}}, {'codex','claude','kimi'})
        self.assertFalse((self.home/'.codex').exists()); self.assertFalse((self.config/'accounts.json').exists())
        self.assertNotIn('TEMPORARY',json.dumps(data))
    def test_create_private_catalog_and_profile(self):
        self.add(); self.assertEqual((self.config/'accounts.json').stat().st_mode & 0o777,0o600)
        self.assertEqual((self.config/'accounts/work').stat().st_mode & 0o777,0o700)
        self.assertNotIn('auth',json.loads((self.config/'accounts.json').read_text())['profiles']['work'])
    def test_stale_revision_and_duplicate_rejected(self):
        before=self.ls()['revision']; self.add()
        self.run_hgs('account','add','other','--provider','kimi','--revision',before,ok=False)
        self.run_hgs('account','add','work','--provider','codex',ok=False)
        self.assertEqual(len([p for p in self.ls()['profiles'] if p['provider'] not in {'dsh','opencode'}]),4)
    def test_permissions_are_revision_checked_and_machine_local(self):
        self.add();native=self.fake_auth();before=(native/'auth.json').read_bytes()
        for account in ['work','native-codex','native-claude','native-kimi','native-dsh']:
            revision=self.ls()['revision']
            data=json.loads(self.run_hgs('account','permissions',account,'--mode','bypass','--revision',revision).stdout)
            self.assertEqual(next(p for p in data['profiles'] if p['id']==account)['permission_mode'],'bypass')
            self.run_hgs('account','permissions',account,'--mode','provider','--revision',revision,ok=False)
            self.run_hgs('account','permissions',account,'--mode','invalid',ok=False)
            self.run_hgs('account','permissions',account,'--mode','provider')
            self.assertEqual(next(p for p in self.ls()['profiles'] if p['id']==account)['permission_mode'],'provider')
        self.run_hgs('account','permissions','missing','--mode','bypass',ok=False)
        self.assertEqual((native/'auth.json').read_bytes(),before)
        self.run_hgs('account','permissions','work','--mode','bypass','--dry-run')
        self.assertEqual(next(p for p in self.ls()['profiles'] if p['id']=='work')['permission_mode'],'provider')

    def test_auto_permission_mode_is_claude_only(self):
        self.add('review','claude');self.add()
        modes={p['id']:p.get('permission_modes') for p in self.ls()['profiles']}
        self.assertEqual(modes['review'],['provider','auto','bypass']);self.assertEqual(modes['work'],['provider','bypass'])
        self.assertEqual(modes['native-kimi'],['provider','bypass']);self.assertFalse(modes.get('native-opencode'))
        self.run_hgs('account','permissions','review','--mode','auto')
        review=next(p for p in self.ls()['profiles'] if p['id']=='review')
        self.assertEqual((review['permission_mode'],review['effective_permission_mode']),('auto','auto'))
        for account in ['work','native-codex','native-kimi','native-dsh']:
            self.run_hgs('account','permissions',account,'--mode','auto',ok=False)
            self.assertEqual(next(p for p in self.ls()['profiles'] if p['id']==account)['permission_mode'],'provider')

    def test_launch_permissions_choose_one_sessions_mode(self):
        claude=self.run_hgs('claude',str(self.root),'--new','--permissions','auto','--dry-run').stdout.decode()
        self.assertIn('--permission-mode auto',claude)
        codex=self.run_hgs('codex',str(self.root),'--new','--permissions','bypass','--dry-run').stdout.decode()
        self.assertIn('--dangerously-bypass-approvals-and-sandbox',codex)
        # Auto is Claude's; DeepSeek and OpenCode modes come from their own settings.
        for agent,mode in [('codex','auto'),('kimi','auto'),('dsh','bypass'),('opencode','bypass'),('claude','provider')]:
            self.run_hgs(agent,str(self.root),'--new','--permissions',mode,'--dry-run',ok=False)
        self.run_hgs('claude',str(self.root),'--new','--permissions','auto','--dry-run','--','--permission-mode','plan',ok=False)
        self.assertNotIn('--permission-mode',self.run_hgs('claude',str(self.root),'--new','--dry-run').stdout.decode())

    @unittest.skipIf(os.name == 'nt' or not shutil.which('node'), 'the Kimi helper starts a POSIX mock through node')
    def test_kimi_usage_uses_the_running_native_server(self):
        import http.server, threading
        routes = {}
        class Native(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_GET(self):
                body = routes.get(self.path) if self.headers.get('Authorization') == 'Bearer fixturetoken0123456789' else None
                self.send_response(200 if body is not None else 404); self.send_header('Content-Type', 'application/json'); self.end_headers()
                self.wfile.write(json.dumps(body if body is not None else {'statusCode': 404}).encode())
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Native)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        # Kimi Code 0.24 reuses its running server and ignores --port, then exits.
        (self.bin/'kimi').write_text('#!/bin/sh\n'
            f'echo "A server is already running at http://127.0.0.1:{server.server_port} — the options from this command were not applied."\n'
            f'echo "  Local:    http://127.0.0.1:{server.server_port}/#token=fixturetoken0123456789"\n')
        credentials = self.home/'.kimi-code/credentials'; credentials.mkdir(parents=True)
        (credentials/'kimi-code.json').write_text(json.dumps({'access_token': 'expired-fixture', 'refresh_token': 'refresh-fixture', 'expires_at': 1}))
        def inspect():
            return json.loads(self.run_hgs('account', 'inspect', 'native-kimi', '--refresh').stdout)
        # Routes this Kimi version no longer serves leave the grant expired, and say so.
        self.assertEqual(inspect()['status'], 'expired')
        routes['/api/v1/oauth/usage'] = {'data': {'kind': 'ok', 'quota': {'usages': {'limit5h': {'usedRatio': 0.25, 'resetAt': None}}}}}
        routes['/api/v1/oauth/userinfo'] = {'data': {'kind': 'ok', 'userInfo': {'nickname': 'Fixture', 'email': 'kimi@example.invalid', 'userLevelName': 'Pro'}}}
        data = inspect()
        self.assertEqual((data['status'], data['identity']['email']), ('ok', 'kimi@example.invalid'))
        self.assertEqual(data['windows'][0]['used_percent'], 25.0)
        self.assertEqual(data['auth_status'], 'signed_in')
        # A plan without a quota report still proves the sign-in, and the grant stays
        # unavailable rather than expired: the native server answered with a live token.
        routes.pop('/api/v1/oauth/usage')
        data = inspect()
        self.assertEqual((data['status'], data['auth_status']), ('unavailable', 'signed_in'))
        self.assertEqual((data['identity']['email'], data['windows']), ('kimi@example.invalid', []))
        # A failed identity read is never reported as a sign-in, even with a quota.
        routes['/api/v1/oauth/usage'] = {'data': {'kind': 'ok', 'quota': {'usages': {'limit5h': {'usedRatio': 0.25, 'resetAt': None}}}}}
        routes.pop('/api/v1/oauth/userinfo')
        data = inspect()
        self.assertNotIn('auth_status', data)
        self.assertEqual(data['status'], 'ok')

    def test_ollama_account_runs_claude_code_on_a_local_model(self):
        import http.server, threading
        class Ollama(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_GET(self):
                body = {'models': [{'name': 'qwen3.8-cc:latest'}, {'name': 'qwen2.5-coder:7b'}]} if self.path == '/api/tags' else None
                self.send_response(200 if body else 404); self.send_header('Content-Type', 'application/json'); self.end_headers()
                self.wfile.write(json.dumps(body or {}).encode())
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Ollama)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        endpoint = f'http://127.0.0.1:{server.server_port}'
        listed = json.loads(self.run_hgs('account', 'ollama-models', '--endpoint', endpoint).stdout)
        self.assertEqual(listed['models'], ['qwen2.5-coder:7b', 'qwen3.8-cc:latest'])
        self.run_hgs('account', 'add', 'local', '--provider', 'codex', '--ollama', 'qwen3.8-cc:latest', ok=False)
        self.run_hgs('account', 'add', 'local', '--provider', 'claude', '--ollama', 'two words', ok=False)
        self.run_hgs('account', 'add', 'local', '--provider', 'claude', '--ollama', 'm', '--endpoint', 'https://example.invalid', ok=False)
        self.assertFalse((self.config/'accounts/local').exists())
        self.run_hgs('account', 'add', 'local', '--provider', 'claude', '--label', 'Local Qwen', '--ollama', 'qwen3.8-cc:latest',
                     '--endpoint', endpoint, '--context', '262144')
        home = self.config/'accounts/local'
        self.assertEqual(json.loads((home/'.hgs-local-model.json').read_text())['model'], 'qwen3.8-cc:latest')
        self.assertTrue(json.loads((home/'.claude.json').read_text())['hasCompletedOnboarding'])
        status = json.loads(self.run_hgs('account', 'inspect', 'local', '--refresh').stdout)
        self.assertEqual((status['status'], status['identity']['plan'], status['reachable']), ('configured', 'qwen3.8-cc:latest', True))
        launch = self.run_hgs('claude', str(self.root), '--new', '--account', 'local', '--dry-run').stdout.decode()
        for expected in (f'ANTHROPIC_BASE_URL={endpoint}', 'ANTHROPIC_AUTH_TOKEN=ollama', 'ANTHROPIC_DEFAULT_OPUS_MODEL=qwen3.8-cc:latest',
                         'CLAUDE_CODE_SUBAGENT_MODEL=qwen3.8-cc:latest', 'CLAUDE_CODE_MAX_CONTEXT_TOKENS=262144'):
            self.assertIn(expected, launch)
        # An ordinary Claude account keeps Anthropic's endpoint.
        self.assertNotIn('ANTHROPIC_BASE_URL=http', self.run_hgs('claude', str(self.root), '--new', '--dry-run').stdout.decode())

    def test_remove_preserves_native_context(self):
        self.add(); f=self.config/'accounts/work/session.jsonl'; f.write_text('saved context')
        self.run_hgs('account','rm','work'); self.assertEqual(f.read_text(),'saved context')
        original=self.fake_auth();auth=(original/'auth.json').read_bytes()
        self.run_hgs('account','rename','native-codex','--label','Personal')
        before=self.ls()['revision']
        self.run_hgs('account','rm','native-codex','--revision',before)
        removed=self.ls();self.assertNotIn('native-codex',{p['id'] for p in removed['profiles']})
        self.assertEqual(next(p for p in removed['removed_profiles'] if p['id']=='native-codex')['label'],'Personal')
        self.assertEqual((original/'auth.json').read_bytes(),auth)
        self.run_hgs('account','restore','native-codex','--revision',before,ok=False)
        self.run_hgs('account','restore','native-codex','--revision',removed['revision'])
        self.assertEqual(next(p for p in self.ls()['profiles'] if p['id']=='native-codex')['label'],'Personal')
        self.run_hgs('account','rm','native-fake',ok=False)
        self.run_hgs('account','restore','unknown',ok=False)
        self.run_hgs('account','restore','work')
        self.assertEqual(f.read_text(),'saved context')
        self.run_hgs('account','rm','work')
        self.run_hgs('account','add','work','--provider','codex',ok=False)

    def test_defaults_are_provider_scoped_revision_checked_and_explicitly_overridden(self):
        self.add('work','claude');self.add('code','codex')
        revision=self.ls()['revision']
        selected=json.loads(self.run_hgs('account','default','work','--revision',revision).stdout)
        self.assertEqual(selected['defaults'],{'claude':'work'})
        self.assertEqual([p['id'] for p in selected['profiles'] if p['provider']=='claude' and p['is_default']],['work'])
        self.run_hgs('account','default','code','--revision',revision,ok=False)
        out=self.run_hgs('claude',str(self.root),'--new','--dry-run').stdout.decode()
        self.assertIn('CLAUDE_CONFIG_DIR='+str(self.config/'accounts/work'),out)
        self.assertIn('HGS_ACCOUNT_ID=work',out)
        native=self.run_hgs('claude',str(self.root),'--new','--account','native-claude','--dry-run').stdout.decode()
        self.assertIn('HGS_ACCOUNT_ID=native-claude',native);self.assertNotIn('accounts/work',native)
        code=self.run_hgs('codex',str(self.root),'--new','--dry-run').stdout.decode()
        self.assertNotIn('accounts/work',code)
        self.run_hgs('account','default','code','--dry-run');self.assertEqual(self.ls()['defaults'],{'claude':'work'})
        self.run_hgs('account','default','code');self.assertEqual(self.ls()['defaults'],{'claude':'work','codex':'code'})
        self.run_hgs('account','default','missing',ok=False)
        self.run_hgs('account','default','native-kimi')
        self.run_hgs('account','rm','work');self.assertNotIn('claude',self.ls()['defaults'])
        self.run_hgs('account','default','work',ok=False)
        self.run_hgs('account','restore','work');self.assertNotIn('claude',self.ls()['defaults'])
        self.run_hgs('account','default','native-claude');self.assertEqual(self.ls()['defaults']['claude'],'native-claude')
    def test_rename_preserves_ids_credentials_and_checks_revision(self):
        original=self.fake_auth(); auth=(original/'auth.json').read_bytes(); self.add()
        before=self.ls()['revision']
        result=json.loads(self.run_hgs('account','rename','native-codex','--label','Personal','--revision',before).stdout)
        native=next(p for p in result['profiles'] if p['id']=='native-codex')
        self.assertEqual(native['label'],'Personal');self.assertEqual((original/'auth.json').read_bytes(),auth)
        self.assertFalse((self.config/'accounts/native-codex').exists())
        self.run_hgs('account','rename','work','--label','Company','--revision',before,ok=False)
        self.run_hgs('account','rename','work','--label','Company')
        self.assertEqual(next(p for p in self.ls()['profiles'] if p['id']=='work')['label'],'Company')
        for bad in ['','  ','bad\nname']:
            self.run_hgs('account','rename','work','--label',bad,ok=False)
        self.run_hgs('account','rename','unknown','--label','Other',ok=False)

    def test_native_account_probe_is_scoped_sanitized_and_cached(self):
        self.add();self.env['OPENAI_API_KEY']='PARENT-SECRET'
        cmd=self.bin/'codex'
        cmd.write_text('''#!/usr/bin/env python3
import json,os,sys,pathlib
home=pathlib.Path(os.environ['CODEX_HOME'])
assert home.name=='work' and not os.environ.get('OPENAI_API_KEY')
count=home/'probes';count.write_text(str(int(count.read_text())+1) if count.exists() else '1')
for line in sys.stdin:
 r=json.loads(line);rid=r.get('id')
 if rid is None:continue
 if r['method']=='initialize':value={}
 elif r['method']=='account/read':value={'account':{'email':'work@example.test','planType':'pro','type':'chatgpt','token':'DO-NOT-EXPOSE'}}
 elif r['method']=='account/rateLimits/read':value={'rateLimits':{'primary':{'usedPercent':72,'windowDurationMins':300,'resetsAt':1900000000}}}
 else:raise RuntimeError('unexpected method')
 print(json.dumps({'id':rid,'result':value}),flush=True)
''')
        cmd.chmod(0o755)
        response=self.run_hgs('account','inspect','work');data=json.loads(response.stdout)
        self.assertNotIn(b'DO-NOT-EXPOSE',response.stdout);self.assertNotIn(b'PARENT-SECRET',response.stdout)
        self.assertEqual(data['identity']['email'],'work@example.test')
        self.assertEqual(data['windows'][0]['used_percent'],72)
        self.assertEqual(data['windows'][0]['window_minutes'],300)
        self.run_hgs('account','inspect','work');home=self.config/'accounts/work'
        self.assertEqual((home/'probes').read_text(),'1')
        catalog=self.ls(); row=next(p for p in catalog['profiles'] if p['id']=='work')
        self.assertEqual(row['account_status']['identity']['email'],'work@example.test')
        self.assertEqual(row['account_status']['windows'][0]['used_percent'],72)
        self.assertEqual(row['account_status']['windows'][0]['window_minutes'],300)
        self.assertEqual((home/'probes').read_text(),'1') # listing never contacts the provider
        self.assertNotIn('DO-NOT-EXPOSE',json.dumps(catalog))
        (home/'auth.json').write_text('{"tokens":{"access_token":"CHANGED"}}')
        self.assertIsNone(next(p for p in self.ls()['profiles'] if p['id']=='work')['account_status'])
        self.run_hgs('account','inspect','work');self.assertEqual((home/'probes').read_text(),'2')
        for f in (self.config/'account-status').glob('*.json'):
            self.assertEqual(f.stat().st_mode & 0o777,0o600)
            self.assertNotIn('DO-NOT-EXPOSE',f.read_text());self.assertNotIn('CHANGED',f.read_text())
    def test_explicit_agent_install_is_allowlisted_and_failed_download_never_runs(self):
        # Unix runs npm or the vendors' install.sh; Windows gets install.ps1 (src/accounts/install.rs).
        for provider,package,url in (('codex','@openai/codex',''),('claude','','https://claude.ai/install.sh'),
                                     ('kimi','','https://code.kimi.com/kimi-code/install.sh'),('dsh','@deepseek-ai/dsh@0.2.1-rc.2','')):
            result=json.loads(self.run_hgs('account','install',provider,'--dry-run').stdout)
            self.assertEqual(result,{'provider':provider,'package':package,'installer_url':url,'dry_run':True})
        self.run_hgs('account','install','arbitrary-command',ok=False)
        npm=self.bin/'npm'
        npm.write_text('#!/usr/bin/env python3\nimport json,pathlib,sys\npathlib.Path(__file__).with_name("npm-args").write_text(json.dumps(sys.argv[1:]))\n')
        npm.chmod(0o755)
        self.run_hgs('account','install','codex')
        self.assertEqual(json.loads((self.bin/'npm-args').read_text()),['install','-g','@openai/codex'])
        curl=self.bin/'curl'; marker=self.root/'installer-ran'; self.env['TMPDIR']=str(self.root)
        curl.write_text('#!/usr/bin/env python3\nimport pathlib,sys\np=pathlib.Path(sys.argv[sys.argv.index("--output")+1])\np.with_name("curl-url").write_text(sys.argv[-1])\np.write_text('+repr('printf installed > '+str(marker)+'\n')+')\nsys.exit(22)\n')
        curl.chmod(0o755)
        self.run_hgs('account','install','kimi',ok=False)
        self.assertFalse(marker.exists())
        curl.write_text(curl.read_text().replace('sys.exit(22)','sys.exit(0)'))
        self.run_hgs('account','install','kimi')
        self.assertEqual(marker.read_text(),'installed')
        self.run_hgs('account','install','claude')
        self.assertEqual((self.root/'curl-url').read_text(),'https://claude.ai/install.sh')
        self.assertEqual(list(self.root.glob('.tmp*')),[]) # The downloaded script is removed.

    def test_claude_signed_out_json_uses_exit_one(self):
        cmd=self.bin/'claude'
        cmd.write_text('#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps({"loggedIn":False,"authMethod":"none","token":"DO-NOT-EXPOSE"}))\nsys.exit(1)\n')
        cmd.chmod(0o755)
        response=self.run_hgs('account','inspect','native-claude')
        self.assertEqual(json.loads(response.stdout)['status'],'signed_out')
        self.assertNotIn(b'DO-NOT-EXPOSE',response.stdout)
        self.assertFalse((self.home/'.claude').exists())

    def test_profile_launch_and_login_use_native_home(self):
        self.add(); self.env['OPENAI_API_KEY']='PARENT-SECRET'
        out=self.run_hgs('codex',str(self.root),'--new','-n','new','--account','work','--dry-run').stdout.decode()
        self.assertIn('CODEX_HOME='+str(self.config/'accounts/work'),out)
        self.assertIn('OPENAI_API_KEY=',out); self.assertNotIn('PARENT-SECRET',out)
        self.assertIn('exec env',out)
        login=json.loads(self.run_hgs('account','login','work','--dry-run').stdout)
        self.assertEqual(login['home'],str(self.config/'accounts/work')); self.assertEqual(login['arguments'],['login'])
        self.run_hgs('kimi',str(self.root),'--account','work','--dry-run',ok=False)
    def test_claude_continue_uses_selected_profile_history(self):
        self.add('work','claude')
        escaped=''.join(c if c.isascii() and c.isalnum() else '-' for c in str(self.root))
        directory=self.config/'accounts/work/projects'/escaped; directory.mkdir(parents=True)
        (directory/'history.jsonl').write_text('{}\n')
        out=self.run_hgs('claude',str(self.root),'-c','-n','new-name','--account','work','--dry-run').stdout.decode()
        self.assertIn('claude -c',out)
        self.assertFalse((self.home/'.claude/projects').exists())
    def test_native_login_preserves_unset_home_and_managed_claude_uses_subscription(self):
        self.fake_auth()
        provider=self.bin/'codex'
        provider.write_text('#!/usr/bin/env python3\nimport json,os\nprint(json.dumps({"home":os.environ.get("CODEX_HOME")}))\n')
        result=json.loads(self.run_hgs('account','login','native-codex').stdout)
        self.assertIsNone(result['home'])
        self.add('claude-work','claude')
        result=json.loads(self.run_hgs('account','login','claude-work','--dry-run').stdout)
        self.assertEqual(result['arguments'],['auth','login','--claudeai'])
    def claude_login_fixture(self):
        self.add('claude-work','claude')
        home=self.config/'accounts/claude-work'
        settings={'oauthAccount':{'emailAddress':'work@example.test'}, 'projects':{'/untrusted':{'hasTrustDialogAccepted':False}}, 'theme':'dark','unrelated':{'keep':True}}
        (home/'.claude.json').write_text(json.dumps(settings))
        (home/'.credentials.json').write_text('TEMPORARY-UNCHANGED-CREDENTIALS')
        (home/'settings.json').write_text('TEMPORARY-UNCHANGED-PERMISSIONS')
        (self.bin/'claude').write_text('''#!/usr/bin/env python3
import json,os,sys,pathlib
home=pathlib.Path(os.environ['CLAUDE_CONFIG_DIR'])
assert home.name=='claude-work'
assert os.environ['ANTHROPIC_CONFIG_DIR']==str(home/'anthropic')
assert not os.environ.get('ANTHROPIC_API_KEY')
for key in ['CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR','CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR','CLAUDE_CODE_OAUTH_REFRESH_TOKEN','CLAUDE_CODE_OAUTH_SCOPES','CLAUDE_SECURESTORAGE_CONFIG_DIR']:
 assert not os.environ.get(key),key
if sys.argv[1:3]==['auth','login']:
 assert sys.argv[3:]==['--claudeai']
 sys.exit(1 if (home/'fail-login').exists() else 0)
assert sys.argv[1:]==['auth','status','--json']
print(json.dumps({'loggedIn':not (home/'signed-out').exists(),'authMethod':'claude.ai'}))
''')
        return home,settings

    def test_native_claude_login_requires_verified_status(self):
        (self.bin/'claude').write_text('''#!/usr/bin/env python3
import json,os,pathlib,sys
assert not os.environ.get('CLAUDE_CONFIG_DIR')
if sys.argv[1:]==['auth','login']:sys.exit(0)
assert sys.argv[1:]==['auth','status','--json']
signed_in=(pathlib.Path.home()/'signed-in').exists()
print(json.dumps({'loggedIn':signed_in,'authMethod':'claude.ai' if signed_in else 'none'}))
sys.exit(0 if signed_in else 1)
''')
        failed=self.run_hgs('account','login','native-claude',ok=False)
        self.assertIn(b'setup is incomplete',failed.stderr)
        (self.home/'signed-in').touch()
        success=self.run_hgs('account','login','native-claude')
        self.assertIn(b'Sign-in verified',success.stderr)
        self.assertFalse((self.home/'.claude.json').exists())

    def test_claude_login_completes_setup_without_trusting_projects(self):
        home,settings=self.claude_login_fixture()
        native=self.home/'.claude.json';native.write_text('{"native":"unchanged"}')
        self.env['ANTHROPIC_API_KEY']='WRONG-PARENT-ACCOUNT'
        for key in ['CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR','CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR','CLAUDE_CODE_OAUTH_REFRESH_TOKEN','CLAUDE_CODE_OAUTH_SCOPES','CLAUDE_SECURESTORAGE_CONFIG_DIR']:
            self.env[key]='WRONG-PARENT-ACCOUNT'
        response=self.run_hgs('account','login','claude-work')
        settings['hasCompletedOnboarding']=True
        self.assertEqual(json.loads((home/'.claude.json').read_text()),settings)
        self.assertEqual((home/'.credentials.json').read_text(),'TEMPORARY-UNCHANGED-CREDENTIALS')
        self.assertEqual((home/'settings.json').read_text(),'TEMPORARY-UNCHANGED-PERMISSIONS')
        self.assertEqual(native.read_text(),'{"native":"unchanged"}')
        self.assertEqual((home/'.claude.json').stat().st_mode & 0o777,0o600)
        self.assertFalse((home/'.claude.json.lock').exists())
        self.assertNotIn(b'TEMPORARY',response.stdout+response.stderr)
        before=(home/'.claude.json').stat().st_mtime_ns
        self.run_hgs('account','login','claude-work')
        self.assertEqual((home/'.claude.json').stat().st_mtime_ns,before)

    def test_claude_failed_or_unverified_login_does_not_complete_setup(self):
        home,settings=self.claude_login_fixture()
        (home/'fail-login').touch()
        self.run_hgs('account','login','claude-work',ok=False)
        self.assertEqual(json.loads((home/'.claude.json').read_text()),settings)
        (home/'fail-login').unlink();(home/'signed-out').touch()
        result=self.run_hgs('account','login','claude-work',ok=False)
        self.assertIn(b'setup is incomplete',result.stderr)
        self.assertEqual(json.loads((home/'.claude.json').read_text()),settings)

    def test_claude_setup_preserves_corrupt_or_locked_or_linked_config(self):
        home,settings=self.claude_login_fixture();path=home/'.claude.json'
        path.write_text('broken json')
        self.run_hgs('account','login','claude-work',ok=False)
        self.assertEqual(path.read_text(),'broken json');self.assertFalse((home/'.claude.json.lock').exists())
        path.write_text(json.dumps(settings));(home/'.claude.json.lock').mkdir()
        self.run_hgs('account','login','claude-work',ok=False)
        self.assertEqual(json.loads(path.read_text()),settings);self.assertTrue((home/'.claude.json.lock').is_dir())
        (home/'.claude.json.lock').rmdir();path.unlink()
        native=self.home/'.claude.json';native.write_text(json.dumps(settings));path.symlink_to(native)
        self.run_hgs('account','login','claude-work',ok=False)
        self.assertTrue(path.is_symlink());self.assertEqual(json.loads(native.read_text()),settings)

    def test_claude_sign_in_status_does_not_depend_on_quota_availability(self):
        home,_=self.claude_login_fixture()
        result=json.loads(self.run_hgs('account','inspect','claude-work','--refresh').stdout)
        self.assertEqual(result['auth_status'],'signed_in')
        self.assertEqual(result['status'],'unavailable')
        row=next(p for p in self.ls()['profiles'] if p['id']=='claude-work')
        self.assertEqual(row['account_status']['auth_status'],'signed_in')

    def test_expired_claude_usage_token_does_not_expire_native_sign_in(self):
        home,_=self.claude_login_fixture()
        credentials=home/'.credentials.json'
        credentials.write_text(json.dumps({'claudeAiOauth':{'expiresAt':1,'accessToken':'SYNTHETIC-EXPIRED-TOKEN'}}))
        before=credentials.read_bytes()
        # No usage request or refresh is needed when the saved access token is old.
        marker=self.root/'unexpected-network'
        curl=self.bin/'curl';curl.write_text('#!/bin/sh\nprintf unexpected > '+str(marker)+'\nexit 99\n');curl.chmod(0o755)
        result=json.loads(self.run_hgs('account','inspect','claude-work','--refresh').stdout)
        # WhiteWh reports an expired quota token explicitly while preserving the
        # independently verified native sign-in (12848a65).
        self.assertEqual((result['auth_status'],result['status']),('signed_in','expired'))
        self.assertNotIn('SYNTHETIC-EXPIRED-TOKEN', json.dumps(result))
        row=next(p for p in self.ls()['profiles'] if p['id']=='claude-work')
        self.assertEqual((row['account_status']['auth_status'],row['account_status']['status']),
                         ('signed_in','expired'))
        self.assertEqual(result['windows'],[])
        self.assertEqual(credentials.read_bytes(),before)
        self.assertFalse(marker.exists())
        (home/'signed-out').touch()
        result=json.loads(self.run_hgs('account','inspect','claude-work','--refresh').stdout)
        self.assertEqual(result['auth_status'],'signed_out')
        self.assertEqual(result['status'],'signed_out')

    @unittest.skipUnless(os.uname().sysname=='Darwin', 'macOS Keychain diagnostic')
    def test_claude_inaccessible_saved_sign_in_preserves_profile_identity(self):
        import hashlib
        home,_=self.claude_login_fixture();(home/'signed-out').touch()
        expected='Claude Code-credentials-'+hashlib.sha256(str(home).encode()).hexdigest()[:8]
        (self.bin/'security').write_text('#!/usr/bin/env python3\nimport sys\nassert sys.argv[1:]=='+repr(['find-generic-password','-s',expected])+'\nprint("metadata only")\n')
        result=self.run_hgs('account','inspect','claude-work','--refresh')
        data=json.loads(result.stdout)
        self.assertIn(data['status'],['credentials_locked','desktop_session_unavailable','credentials_unavailable'])
        self.assertEqual(data['identity']['email'],'work@example.test')
        self.assertTrue(data['identity_cached'])
        self.assertNotIn('TEMPORARY',result.stdout.decode())

    def test_account_copy_local_real_pipe(self):
        original=self.fake_auth(); (original/'config.toml').write_text('model = "private-model"\nnotify = ["do-not-copy"]\n')
        receipt=self.run_hgs('account','copy','native-codex','--to','@local','--as','copied','--label','Copy')
        self.assertNotIn(b'TEMPORARY-TEST-KEY',receipt.stdout+receipt.stderr)
        copied=self.config/'accounts/copied'; self.assertEqual((copied/'auth.json').read_bytes(),(original/'auth.json').read_bytes())
        self.assertNotIn('notify',(copied/'config.toml').read_text())
        self.assertEqual((copied/'auth.json').stat().st_mode & 0o777,0o600)
        self.run_hgs('account','copy','native-codex','--to','@local','--as','copied',ok=False)
    def test_kimi_only_auth_and_provider_config_copied(self):
        d=self.home/'.kimi-code'; (d/'credentials').mkdir(parents=True)
        (d/'config.toml').write_text('default_model="test"\nnotify=["evil"]\n[[hooks]]\ncommand="evil"\n[providers.test]\ntype="openai"\napi_key="TEMPORARY"\n[models.test]\nprovider="test"\nmodel="test"\n')
        (d/'credentials/default.json').write_text('{"token":"TEMPORARY"}')
        (d/'history.jsonl').write_text('private conversations')
        self.run_hgs('account','copy','native-kimi','--to','@local','--as','kimi-copy')
        dest=self.config/'accounts/kimi-copy'; config=(dest/'config.toml').read_text()
        self.assertIn('providers.test',config); self.assertNotIn('notify',config); self.assertNotIn('hooks',config)
        self.assertTrue((dest/'credentials/default.json').exists()); self.assertFalse((dest/'history.jsonl').exists())
    def test_import_rejects_traversal_or_unrelated_settings(self):
        for name,content in [('../escape','{}'),('config.toml','notify=["bad"]')]:
            payload=json.dumps({'version':1,'provider':'codex','files':{name:base64.b64encode(content.encode()).decode()}}).encode()
            self.run_hgs('account','_import','bad',input=payload,ok=False)
        self.assertFalse((self.config/'accounts/bad').exists())
    def test_export_rejects_external_symlink_and_claude(self):
        d=self.home/'.codex'; d.mkdir(); outside=self.root/'outside'; outside.write_text('{"token":"outside"}'); (d/'auth.json').symlink_to(outside)
        self.run_hgs('account','copy','native-codex','--to','@local','--as','bad',ok=False)
        self.run_hgs('account','copy','native-claude','--to','@local','--as','bad',ok=False)
    def test_ssh_helper_cannot_hold_transfer_pipe_open_after_parent_exits(self):
        self.env['HGS_PEERS']='remote'
        payload=json.dumps({'version':1,'provider':'codex','files':{
            'auth.json':base64.b64encode(b'{"OPENAI_API_KEY":"TEMPORARY-TEST-KEY"}').decode(),
            'config.toml':base64.b64encode(b'cli_auth_credentials_store="file"\n').decode()}})
        script=self.bin/'ssh'
        script.write_text('#!/usr/bin/env python3\nimport subprocess,sys\nsubprocess.Popen(["sleep","30"])\nprint('+repr(payload)+',flush=True)\n')
        script.chmod(0o755)
        start=time.monotonic()
        result=self.run_hgs('account','copy','native-codex','--from','remote','--to','@local','--as','copied')
        self.assertLess(time.monotonic()-start,5)
        self.assertNotIn(b'TEMPORARY-TEST-KEY',result.stdout+result.stderr)
        self.assertTrue((self.config/'accounts/copied/auth.json').exists())
    def test_escaped_ssh_helper_cannot_extend_transfer_deadline(self):
        self.env['HGS_PEERS']='remote'
        pid_file=self.root/'escaped-helper.pid'
        script=self.bin/'ssh'
        script.write_text('#!/usr/bin/env python3\nimport pathlib,subprocess\np=subprocess.Popen(["sleep","60"],start_new_session=True)\npathlib.Path('+repr(str(pid_file))+').write_text(str(p.pid))\nprint("{}",flush=True)\n')
        script.chmod(0o755)
        start=time.monotonic()
        try:
            result=subprocess.run([HGS,'account','copy','native-codex','--from','remote','--to','@local','--as','copied'],env=self.env,cwd=self.root,capture_output=True,timeout=26)
            self.assertNotEqual(result.returncode,0)
            self.assertLess(time.monotonic()-start,25)
            self.assertIn(b'timed out',result.stderr)
            self.assertFalse((self.config/'accounts/copied').exists())
        finally:
            if pid_file.exists():
                try: os.kill(int(pid_file.read_text()),signal.SIGTERM)
                except ProcessLookupError: pass
    def test_invalid_ids_are_not_paths(self):
        for id in ('../bad','native-fake','-oops','with space'):
            self.run_hgs('account','add',id,'--provider','codex',ok=False)
        self.assertFalse((self.config/'accounts').exists())


class OpenCodeUserInstall(unittest.TestCase):
    """OpenCode's official user install, ~/.opencode/bin, is found after every other
    PATH folder. The caller's PATH holds only a private folder: /usr/bin can hold a
    real OpenCode (an Arch package does), and a managed GUI runs hgs with a minimal
    PATH. hgs puts ~/.local/bin, /opt/homebrew/bin and /usr/local/bin first, so an
    OpenCode in either system folder would mask these cases."""
    def setUp(self):
        for folder in ('/opt/homebrew/bin', '/usr/local/bin'):
            if os.access(os.path.join(folder, 'opencode'), os.X_OK):
                self.skipTest('An OpenCode in ' + folder + ' precedes every folder these cases control')
        temp = tempfile.TemporaryDirectory(prefix='zerus-opencode-install-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.home = self.root / 'home'
        self.config = self.home / '.config/hgs'
        self.config.mkdir(parents=True)
        # The other agents and tools hgs may probe, never the real ones.
        local = self.home / '.local/bin'
        local.mkdir(parents=True)
        for executable in ('codex', 'claude', 'kimi', 'dsh', 'ssh', 'security', 'tmux'):
            (local / executable).write_text('#!/bin/sh\nexit ' + ('1' if executable == 'tmux' else '99') + '\n')
            (local / executable).chmod(0o755)
        self.caller = self.root / 'caller-bin'
        self.caller.mkdir()
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(('HGS_', 'OPENCODE_', 'XDG_'))
                    and key not in ('CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'KIMI_CODE_HOME', 'TMUX', 'TMUX_PANE')}
        self.env.update(HOME=str(self.home), PATH=str(self.caller), HGS_CONFIG_DIR=str(self.config),
                        HGS_STATE_DIR=str(self.root / 'state'), HGS_SELF='test', HGS_PEERS='', HGS_TAB='0')
        self.user_install = self.home / '.opencode/bin'

    def fake_opencode(self, folder, version):
        """An `opencode` that logs every argument list it receives and answers only
        `--version`; any other command fails. Returns its log."""
        folder.mkdir(parents=True, exist_ok=True)
        log = folder / 'calls.log'
        (folder / 'opencode').write_text(
            '#!/bin/sh\nprintf "%s\\n" "$*" >> "' + str(log) + '"\n'
            '[ "$#" -eq 1 ] && [ "$1" = --version ] && echo "opencode v' + version + '" && exit 0\nexit 9\n')
        (folder / 'opencode').chmod(0o755)
        return log

    def accounts(self):
        run = subprocess.run([HGS, 'account', 'ls'], env=self.env, cwd=self.root, capture_output=True, text=True,
                             timeout=30)
        self.assertEqual(run.returncode, 0, run.stderr)
        return json.loads(run.stdout)

    def opencode_row(self):
        rows = [row for row in self.accounts()['profiles'] if row['id'] == 'native-opencode']
        self.assertEqual(len(rows), 1, 'no native OpenCode row')
        return rows[0]

    def assert_nothing_started(self):
        self.assertFalse((self.home / '.local/state/opencode').exists(), 'a service registration appeared')
        from test_opencode_native import owned_processes
        self.assertEqual(owned_processes(str(self.root)), [])

    def test_the_official_user_install_is_found_and_only_asked_its_version(self):
        calls = self.fake_opencode(self.user_install, '2.0.32')
        row = self.opencode_row()
        self.assertTrue(row['installed'], row.get('install_reason'))
        self.assertNotIn('install_reason', row)
        self.assertEqual((row['catalog_state'], row['model_options']), ('not_running', []))
        self.assertIn('OpenCode is not running', row['catalog_reason'])
        # Listing asked only for the version: no service, API, provider or model command.
        self.assertEqual(calls.read_text().splitlines(), ['--version'])
        self.assert_nothing_started()

    def test_without_any_opencode_there_is_no_native_row(self):
        listed = self.accounts()
        self.assertNotIn('native-opencode', [row['id'] for row in listed['profiles'] + listed['removed_profiles']])
        self.assert_nothing_started()

    def test_an_opencode_earlier_on_path_wins_over_the_user_install(self):
        user = self.fake_opencode(self.user_install, '2.0.32')
        earlier = self.fake_opencode(self.caller, '1.18.35')
        row = self.opencode_row()
        self.assertFalse(row['installed'])
        self.assertEqual(row['install_reason'], 'OpenCode 1.18.35 is not supported; install OpenCode 2.0.32 or newer 2.x')
        self.assertEqual(earlier.read_text().splitlines(), ['--version'])
        self.assertFalse(user.exists(), 'the user install was run')
        self.assert_nothing_started()

if __name__=='__main__': unittest.main()
