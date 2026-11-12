#!/usr/bin/env python3
"""Content-free hook health and explicit repair in owned synthetic homes only."""
import hashlib,json,os,subprocess,sys,tempfile,time,unittest,uuid
from pathlib import Path
import test_input as fixtures
import hook_command_fixtures as shell_probes
HGS=fixtures.HGS
TMUX=fixtures.TMUX

@unittest.skipUnless(os.name=='posix','Unix private executable identity fixture')
class HookHealth(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='hgs-hook-health-');self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.directory=self.root/'.claude';self.directory.mkdir()
        self.config=self.directory/'settings.json';self.state=self.root/'state';self.state.mkdir()
        # hgs prepends HOME/.local/bin and Homebrew before the inherited PATH.
        self.bin=self.root/'.local/bin';self.bin.mkdir(parents=True);self.program=self.bin/'claude'
        self.program.write_text('#!/bin/sh\nprintf invoked > "'+str(self.root/'invoked')+'"\nexit 99\n');self.program.chmod(0o755)
        for provider in ['codex','kimi','opencode','dsh']:
            path=self.bin/provider;path.write_bytes(self.program.read_bytes());path.chmod(0o755)
        self.env=dict(os.environ,HOME=str(self.root),HGS_CONFIG_DIR=str(self.root/'hgs-config'),HGS_STATE_DIR=str(self.state),HGS_SELF='fixture',HGS_PEERS='',PATH=str(self.bin)+':'+os.environ['PATH'])
        for key in ['CLAUDE_CONFIG_DIR','CODEX_HOME','KIMI_CODE_HOME','DSH_HOME','OPENCODE_HOME','TMUX','TMUX_PANE','HGS_RUN_ID','HGS_SESSION','HGS_ACCOUNT_ID','HGS_EXECUTABLE']:self.env.pop(key,None)
    def run_hgs(self,args,body=None,ok=True):
        r=subprocess.run([str(HGS),'hooks',*args],env=self.env,input=json.dumps(body) if body is not None else '',text=True,capture_output=True,timeout=8)
        if ok:self.assertEqual(r.returncode,0,r.stderr);return json.loads(r.stdout)
        self.assertNotEqual(r.returncode,0);return r.stderr
    def inspect(self):return self.run_hgs(['--account','native-claude','--json'])
    def cache(self):
        stat=self.program.stat();identity=f'{self.program.resolve()}\0{stat.st_size}\0{stat.st_mtime_ns}\0{stat.st_dev}:{stat.st_ino}:{stat.st_ctime_ns//1000000000}.{stat.st_ctime_ns%1000000000}\n'
        folder=self.state/'hook-versions';folder.mkdir(exist_ok=True)
        (folder/(hashlib.sha256(str(self.config.resolve()).encode()).hexdigest()+'.json')).write_text(json.dumps(dict(executable=hashlib.sha256(identity.encode()).hexdigest(),version='2.1.296')))
    def repair(self,status,**changes):
        request=dict(request_id=str(uuid.uuid4()),expected_provider='claude',expected_account_id='native-claude',expected_config_sha256=status['configuration']['sha256'],expected_path_id=status['configuration']['path_id']);request.update(changes)
        return ['--account','native-claude','--repair','--json'],request
    def test_read_is_content_free_unknown_and_has_no_filesystem_or_provider_effect(self):
        self.config.write_text(json.dumps(dict(apiKey='synthetic-do-not-disclose',hooks={})))
        before={str(p.relative_to(self.root)):p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        value=self.inspect();self.assertEqual(value['configuration']['state'],'missing');self.assertEqual(value['configuration']['compatibility']['state'],'unknown')
        self.assertEqual(value['trust']['state'],'unknown');self.assertEqual(value['effective']['state'],'unknown');self.assertEqual(value['observed']['state'],'unknown')
        self.assertNotIn('synthetic-do-not-disclose',json.dumps(value));self.assertNotIn(str(self.root),json.dumps(value))
        self.assertEqual(before,{str(p.relative_to(self.root)):p.read_bytes() for p in self.root.rglob('*') if p.is_file()})
        args,body=self.repair(value);self.assertIn('compatibility is unknown',self.run_hgs(args,body,False));self.assertFalse((self.root/'invoked').exists())
        for provider in ['opencode','dsh']:
            api=self.run_hgs(['--account','native-'+provider,'--json']);self.assertEqual(api['configuration']['state'],'not_applicable')
        self.assertFalse((self.root/'invoked').exists())
    def test_repair_preserves_personal_settings_and_legacy_command_then_is_idempotent(self):
        legacy=dict(type='command',command='python3 /opt/fixture/hgs_state.py hook',timeout=5)
        original=dict(apiKey='synthetic-keep',hooks=dict(Stop=[dict(hooks=[dict(type='command',command='personal-hook'),legacy])]))
        self.config.write_text(json.dumps(original));old=self.config.read_bytes();self.cache();status=self.inspect()
        self.assertEqual(status['configuration']['compatibility']['state'],'compatible')
        args,body=self.repair(status);result=self.run_hgs(args,body)
        self.assertEqual(result['status'],'repaired');self.assertFalse(result['running_agents_retrofitted'])
        doc=json.loads(self.config.read_text());self.assertEqual(doc['apiKey'],'synthetic-keep');self.assertEqual(doc['hooks']['Stop'],original['hooks']['Stop']);self.assertEqual(self.config.with_name('settings.json.pre-hgs').read_bytes(),old)
        args,body=self.repair(result);self.assertEqual(self.run_hgs(args,body)['status'],'unchanged');self.assertFalse((self.root/'invoked').exists())
    def wrapper(self):
        """An HGS_EXECUTABLE wrapper with another name that runs this hgs."""
        wrapper = self.bin / 'hgs-wrapper'
        wrapper.write_text('#!/bin/sh\nexec "' + str(HGS) + '" "$@"\n'); wrapper.chmod(0o755)
        return wrapper
    def test_entries_a_wrapper_wrote_stay_hgs_from_a_plain_environment(self):
        # zerus-b64dl: the program an install wrote is recorded for this file,
        # so health and repair from a shell without the wrapper still own it.
        self.config.write_text(json.dumps(dict(model='keep')));self.cache()
        wrapper = self.wrapper()
        plain = dict(self.env)
        self.env['HGS_EXECUTABLE'] = str(wrapper)
        args,body=self.repair(self.inspect());self.assertEqual(self.run_hgs(args,body)['status'],'repaired')
        written = self.config.read_bytes();self.assertIn(str(wrapper).encode(), written)
        self.env = plain
        status = self.inspect()
        self.assertEqual(status['configuration']['state'],'configured')
        self.assertFalse(status['configuration']['unrecognised_tracking'])
        args,body=self.repair(status);self.assertEqual(self.run_hgs(args,body)['status'],'repaired')
        self.assertNotIn(str(wrapper),self.config.read_text())
        self.assertIn(str(HGS),self.config.read_text(),'repair migrates the recorded wrapper to the current runner')
    def test_repair_migrates_recorded_runners_and_preserves_foreign_settings(self):
        for agent in ['claude','codex','kimi']:
            with self.subTest(agent=agent):
                directory=self.root/('.kimi-code' if agent=='kimi' else '.'+agent);directory.mkdir(exist_ok=True)
                path=directory/('config.toml' if agent=='kimi' else 'hooks.json' if agent=='codex' else 'settings.json')
                self.config=path;self.program=self.bin/agent;self.cache()
                foreign='/opt/other-tool notify'
                if agent=='kimi':
                    path.write_text('model = "keep"\n# personal comment\n[[hooks]]\nevent = "Stop"\ncommand = "'+foreign+'"\ncustom = 17\n')
                else:
                    path.write_text(json.dumps(dict(model='keep',permissions=dict(defaultMode='default'),hooks=dict(Stop=[dict(matcher='personal',hooks=[dict(type='command',command=foreign,custom=17)])]))))
                inspect=lambda:self.run_hgs(['--account','native-'+agent,'--json'])
                def repair():
                    status=inspect()['configuration']
                    request=dict(request_id=str(uuid.uuid4()),expected_provider=agent,expected_account_id='native-'+agent,expected_config_sha256=status['sha256'],expected_path_id=status['path_id'])
                    return self.run_hgs(['--account','native-'+agent,'--repair','--json'],request)
                # Real explicit repair first writes and records A, then migrates to B.
                a=self.wrapper();self.env['HGS_EXECUTABLE']=str(a)
                # Personal commands and settings remain outside hgs ownership.
                self.assertFalse(inspect()['configuration']['unrecognised_tracking'])
                self.assertEqual(repair()['status'],'repaired')
                before=path.read_bytes()
                b=self.bin/'new runner';b.write_bytes(a.read_bytes());b.chmod(0o755)
                self.env['HGS_EXECUTABLE']=str(b)
                self.assertEqual(repair()['status'],'repaired')
                text=path.read_text();self.assertNotIn(str(a),text);self.assertIn(str(b),text)
                self.assertIn(foreign,text)
                if agent=='kimi':
                    self.assertIn('# personal comment',text);self.assertIn('custom = 17',text);self.assertIn('model = "keep"',text)
                else:
                    doc=json.loads(text);self.assertEqual(doc['model'],'keep');self.assertEqual(doc['permissions'],dict(defaultMode='default'))
                    self.assertEqual(doc['hooks']['Stop'][0],dict(matcher='personal',hooks=[dict(type='command',command=foreign,custom=17)]))
                record=self.state/'hook-owners'/(hashlib.sha256(str(path.resolve()).encode()).hexdigest()+'.json')
                self.assertEqual(json.loads(record.read_text())['programs'][:2],[str(b),str(a)])
                current=path.read_bytes();self.assertEqual(repair()['status'],'unchanged');self.assertEqual(path.read_bytes(),current)
                self.assertNotEqual(current,before)
    def test_repair_preserves_composed_and_custom_quoted_recorded_hooks(self):
        for agent in ['claude', 'codex', 'kimi']:
            with self.subTest(agent=agent):
                directory = self.root / ('.kimi-code' if agent == 'kimi' else '.' + agent)
                directory.mkdir(exist_ok=True)
                self.config = directory / ('config.toml' if agent == 'kimi' else 'hooks.json' if agent == 'codex' else 'settings.json')
                self.program = self.bin / agent
                self.cache()
                self.config.write_text('model = "keep"\n' if agent == 'kimi' else '{"model":"keep"}')
                account = 'native-' + agent
                def repair():
                    status = self.run_hgs(['--account', account, '--json'])
                    args, body = self.repair(status, expected_provider=agent, expected_account_id=account)
                    args[1] = account
                    return self.run_hgs(args, body)
                a = self.bin / 'hgs-wrapper'
                b = self.bin / 'new runner'
                for path in [a, b]:
                    shell_probes.runner(path, HGS)
                self.env['HGS_EXECUTABLE'] = str(a)
                self.assertEqual(repair()['status'], 'repaired')
                probes = shell_probes.composed(self.root, a)
                shell_probes.preserves_and_runs(self, probes, [v for _, v in probes], self.env)
                shell_probes.append(self.config, agent, probes)
                self.env['HGS_EXECUTABLE'] = str(b)
                self.assertEqual(repair()['status'], 'repaired')
                actual = shell_probes.commands(self.config, agent, len(probes))
                shell_probes.preserves_and_runs(self, probes, actual, self.env)
                self.assertEqual(repair()['status'], 'unchanged')

    def test_foreign_state_arguments_do_not_block_repair_or_become_owned(self):
        foreign = [
            dict(type='command', command='/opt/other-tool __state hook', timeout=7),
            dict(type='command', command='/opt/other-tool __state answer-channel', custom=True),
            dict(type='command', command='/opt/other-tool', args=['__state', 'hook']),
            dict(type='command', command='/opt/other-tool', args=['__state', 'answer-channel']),
        ]
        group = dict(matcher='personal', hooks=foreign)
        for agent in ['claude', 'codex', 'kimi']:
            with self.subTest(agent=agent):
                directory = self.root / ('.kimi-code' if agent == 'kimi' else '.' + agent)
                directory.mkdir(exist_ok=True)
                filename = 'config.toml' if agent == 'kimi' else 'hooks.json' if agent == 'codex' else 'settings.json'
                self.config = directory / filename
                self.program = self.bin / agent
                self.cache()
                if agent == 'kimi':
                    original = 'model = "keep"\n# personal comment\n[[hooks]]\nevent = "Stop"\ncommand = "/opt/other-tool __state hook"\ncustom = 17\n'
                else:
                    original = json.dumps(dict(model='keep', permissions=dict(defaultMode='default'), hooks=dict(Stop=[group])))
                self.config.write_text(original)
                account = 'native-' + agent
                status = self.run_hgs(['--account', account, '--json'])
                args, body = self.repair(status, expected_provider=agent, expected_account_id=account)
                args[1] = account
                result = self.run_hgs(args, body)
                self.assertEqual(result['status'], 'repaired')
                self.assertFalse(status['configuration']['unrecognised_tracking'])
                text = self.config.read_text()
                if agent == 'kimi':
                    self.assertTrue(text.startswith(original), 'foreign Kimi tables and comments keep their bytes')
                else:
                    doc = json.loads(text)
                    self.assertEqual(doc['hooks']['Stop'][0], group)
                    self.assertEqual(doc['model'], 'keep')
                    self.assertEqual(doc['permissions'], dict(defaultMode='default'))
                self.assertFalse(result['configuration']['answer_channel_registered'])
                self.assertFalse((self.root / 'invoked').exists())
    def test_an_unrecorded_hgs_like_entry_blocks_repair_and_is_flagged(self):
        # Written by a wrapper before records existed, or a look-alike: never
        # adopted, rewritten or removed; an explicit repair refuses instead.
        group = dict(hooks=[dict(type='command',command='/opt/old/hgs-wrapper __state hook',timeout=5)])
        self.config.write_text(json.dumps(dict(hooks=dict(SessionStart=[group]))));self.cache()
        before = self.config.read_bytes()
        status = self.inspect()
        self.assertTrue(status['configuration']['unrecognised_tracking'])
        self.assertNotEqual(status['configuration']['state'],'configured')
        args,body=self.repair(status);error=self.run_hgs(args,body,False)
        self.assertIn('HGS_EXECUTABLE',error)
        self.assertEqual(self.config.read_bytes(),before)
        self.assertNotIn('/opt/old/hgs-wrapper',json.dumps(status))
    def test_fresh_repair_adds_only_tracking_and_retains_disabled_native_policy(self):
        self.config.write_text(json.dumps(dict(disableAllHooks=True,permissions=dict(defaultMode='default'))));self.cache()
        args,body=self.repair(self.inspect());result=self.run_hgs(args,body)
        self.assertEqual(result['status'],'repaired');self.assertEqual(result['configuration']['state'],'configured')
        self.assertFalse(result['configuration']['answer_channel_registered']);self.assertTrue(json.loads(self.config.read_text())['disableAllHooks'])
        self.assertEqual(result['effective']['state'],'unknown');self.assertEqual(result['trust']['state'],'unknown')
        self.assertFalse((self.root/'invoked').exists())
    def test_wrong_account_content_symlink_and_malformed_inputs_never_write(self):
        self.config.write_text('{}');self.cache();status=self.inspect()
        for update in [dict(expected_account_id='foreign'),dict(expected_provider='codex')]:
            args,body=self.repair(status,**update);self.run_hgs(args,body,False);self.assertEqual(self.config.read_text(),'{}')
        args,body=self.repair(status);self.config.write_text('{"changed":true}');self.run_hgs(args,body,False);self.assertEqual(self.config.read_text(),'{"changed":true}')
        a=self.directory/'a';b=self.directory/'b';a.write_text('{}');b.write_text('{}');self.config.unlink();self.config.symlink_to(a);self.cache();status=self.inspect();args,body=self.repair(status);self.config.unlink();self.config.symlink_to(b);self.run_hgs(args,body,False);self.assertEqual(a.read_text(),'{}');self.assertEqual(b.read_text(),'{}')
        self.config.unlink();self.config.write_text('[]');self.cache();status=self.inspect();self.assertEqual(status['configuration']['state'],'invalid');args,body=self.repair(status);self.run_hgs(args,body,False);self.assertEqual(self.config.read_text(),'[]')
        self.config.write_bytes(b'x'*(1024*1024+1));self.assertEqual(self.inspect()['configuration']['state'],'unreadable')
        self.assertFalse((self.root/'invoked').exists())
    def test_old_remote_peer_is_not_launched_and_new_peer_keeps_exact_json_stdin(self):
        remote_bin=self.root/'.local/bin';remote_bin.mkdir(parents=True,exist_ok=True)
        remote=remote_bin/'hgs'
        remote.write_text('#!/bin/sh\nif [ "$1" = --help ]; then cat "$HOME/remote-help"; exit 0; fi\ncat > "$HOME/remote-input"\nprintf \'{"read":true}\'\n');remote.chmod(0o755)
        ssh=self.bin/'ssh';ssh.write_text('#!/bin/sh\nfor argument do last="$argument"; done\nexec /bin/sh -c "$last"\n');ssh.chmod(0o755)
        self.env['HGS_PEERS']='old';helpfile=self.root/'remote-help';helpfile.write_text('old help')
        body=json.dumps(dict(request_id=str(uuid.uuid4()),expected_run_id='run',expected_conversation_id='conversation'))
        call=lambda:subprocess.run([str(HGS),'@old','hooks','claude/example/task','--json'],input=body,env=self.env,text=True,capture_output=True,timeout=8)
        old=call();self.assertNotEqual(old.returncode,0);self.assertIn('update the selected computer',old.stderr);self.assertFalse((self.root/'remote-input').exists())
        helpfile.write_text('hooks-health-v1');current=call();self.assertEqual(current.returncode,0,current.stderr);self.assertEqual(json.loads(current.stdout),dict(read=True));self.assertEqual((self.root/'remote-input').read_text(),body)
    def test_session_requires_exact_run_conversation_and_never_scans_history(self):
        name='claude/fixture/task';run=str(uuid.uuid4());conversation=str(uuid.uuid4())
        record=dict(version=1,name=name,agent='claude',run_id=run,conversation_id=conversation,agent_home=str(self.directory),account_id='native-claude',base=['claude'])
        path=self.state/(hashlib.sha256(name.encode()).hexdigest()+'.json');path.write_text(json.dumps(record));before=path.read_bytes()
        body=dict(request_id=str(uuid.uuid4()),expected_run_id=run,expected_conversation_id=conversation)
        session=self.run_hgs([name,'--json'],body);self.assertEqual(session['observed']['state'],'unknown')
        # hooks-health-v1 keeps its keys: the phone connector rejects any other (zerus-b64dl).
        self.assertNotIn('unrecognised_tracking',session['configuration']);self.assertIn('unrecognised_tracking',self.inspect()['configuration'])
        sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'services/mobile'))
        from zerus_mobile.hook_health import metadata_valid
        self.assertTrue(metadata_valid(session))
        for key in ['expected_run_id','expected_conversation_id']:
            bad={**body,key:'foreign'};self.assertIn('identity changed',self.run_hgs([name,'--json'],bad,False))
        self.run_hgs([name,'--json'],{**body,'action':'repair'},False);self.assertEqual(path.read_bytes(),before)
        record['account_id']='native-codex';path.write_text(json.dumps(record));self.assertIn('account identity changed',self.run_hgs([name,'--json'],body,False))

@unittest.skipUnless(TMUX,'private tmux fixture requires tmux')
class HookObservations(unittest.TestCase):
    setUp=fixtures.InputTransport.setUp;tearDown=fixtures.InputTransport.tearDown
    script=fixtures.InputTransport.script;tmux=fixtures.InputTransport.tmux;wait_for=fixtures.InputTransport.wait_for;write_record=fixtures.InputTransport.write_record
    def test_only_verified_current_callbacks_are_reported(self):
        env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=self.run_id,HGS_AGENT='codex')
        for session in [self.conversation_id,'foreign']:
            event=dict(hook_event_name='Stop',session_id=session,last_assistant_message='synthetic-private-text')
            r=subprocess.run([str(HGS),'__state','hook'],input=json.dumps(event),env=env,text=True,capture_output=True,timeout=8);self.assertEqual(r.returncode,0,r.stderr)
        body=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id)
        r=subprocess.run([str(HGS),'hooks',self.name,'--json'],input=json.dumps(body),env=self.env,text=True,capture_output=True,timeout=8);self.assertEqual(r.returncode,0,r.stderr)
        value=json.loads(r.stdout);self.assertEqual(value['observed']['state'],'observed');self.assertEqual([(e['event'],e['count']) for e in value['observed']['events']],[('Stop',1)])
        self.assertNotIn('synthetic-private-text',r.stdout)
