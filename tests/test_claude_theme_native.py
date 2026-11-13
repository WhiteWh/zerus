#!/usr/bin/env python3
"""First-run cosmetic theme through the actual hgs supervisor and official Claude.

The maintained compatibility runner supplies ONE resolved official executable.
Profiles are unauthenticated and never set onboarding, trust or consent bypasses.
The only seeded preference is a normal theme value; its focused AND checked
native row must be observed, so a preference string alone proves nothing.
"""
import hashlib
from collections import deque
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time
import unittest

import test_pause

CLAUDE = os.environ.get('HGS_CLAUDE_TEST_BIN')
TITLE = 'Choose the text style that looks best with your terminal'
Base = type('NativeThemeFixture', (unittest.TestCase,), {
    name: value for name, value in test_pause.Harness.__dict__.items()
    if name not in ('__dict__', '__weakref__') and not name.startswith('test_')})

LOGGER = r'''#!PYTHON
import fcntl,json,os,pathlib,subprocess,sys,time
root=pathlib.Path(ROOT)
args=sys.argv[1:]
if args and args[0]=='send-keys':
    with (root/'theme-keys.jsonl').open('a') as f:f.write(json.dumps(args)+'\n')
started=time.monotonic()
result=subprocess.run([REAL_TMUX,'-S',str(root/'tmux.sock'),'-f','/dev/null',*args],capture_output=True)
ended=time.monotonic()
if args[:1]==['capture-pane']:
    # Diagnostic I/O must neither block a product helper nor alter its result.
    try:
        with (root/'theme-captures.json').open('a+b') as f:
            fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)
            f.seek(0);raw=f.read(65537)
            rows=json.loads(raw) if raw and len(raw)<=65536 else []
            rows.append(dict(source='product',ansi='-e' in args,started=started,ended=ended,returncode=result.returncode,
                             stderr=result.stderr[-1024:].decode(errors='replace')))
            f.seek(0);f.truncate();f.write(json.dumps(rows[-8:]).encode())
    except Exception:pass
if args[:1]==['capture-pane'] and '-e' not in args and TITLE.encode() in result.stdout:
    data=result.stdout[:16384]
    try:
        fd=os.open(root/'first-native-theme.txt',os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
        with os.fdopen(fd,'wb') as f:f.write(data)
        first=True
    except FileExistsError:first=False
    (root/'last-native-theme.txt').write_bytes(data)
    if first and (root/'hold-first-theme').exists():
        end=time.monotonic()+2
        while (root/'hold-first-theme').exists() and time.monotonic()<end:time.sleep(.005)
sys.stdout.buffer.write(result.stdout);sys.stderr.buffer.write(result.stderr)
sys.exit(result.returncode)
'''


@unittest.skipUnless(CLAUDE and test_pause.TMUX,
                     'Set HGS_CLAUDE_TEST_BIN for official Claude first-run theme coverage')
class NativeClaudeTheme(Base):
    def setUp(self):
        super().setUp()
        # Harness has already removed inherited HGS identity/account context.
        # Native auth/config variables are removed too: no owner credential or
        # onboarding state is ever copied into this first-run profile.
        for key in list(self.env):
            if key.startswith(('ANTHROPIC_', 'CLAUDE', 'OPENAI_', 'CODEX_', 'KIMI_', 'OPENCODE_', 'AWS_', 'GOOGLE_')):
                del self.env[key]
        self.env.update(CLAUDE_CONFIG_DIR=str(self.root/'.claude'),
                        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', DISABLE_AUTOUPDATER='1',
                        ANTHROPIC_BASE_URL='http://127.0.0.1:1', TERM='xterm-256color', LANG='C.UTF-8')
        for name, child in [('XDG_CONFIG_HOME','.config'),('XDG_DATA_HOME','.local/share'),
                            ('XDG_CACHE_HOME','.cache'),('XDG_STATE_HOME','.local/state')]:
            self.env[name]=str(self.root/child)
        self.script('claude','#!/bin/sh\nexec '+shlex.quote(str(Path(CLAUDE).resolve()))+' "$@"\n')
        source=LOGGER.replace('#!PYTHON','#!'+sys.executable)
        source=source.replace('ROOT',repr(str(self.root))).replace('REAL_TMUX',repr(test_pause.TMUX)).replace('TITLE.encode()',repr(TITLE)+'.encode()')
        self.script('tmux',source)
        self.name='claude/p/theme'
        self.original_child={}
        self.failure_recorded=False
        self.test_captures=deque(maxlen=8)

    def remember_child(self):
        # Pin once; a replacement binding never supplies another child's exit.
        if self.original_child:return
        try:
            record=self.binding(self.name)
            fields=('session_id','run_id','pid','process_start','pane')
            if all(isinstance(record.get(key),(str,int)) and record[key]
                   and len(str(record[key]))<=256 for key in fields):
                self.original_child={key:record[key] for key in fields}
        except Exception:pass

    def failure_diagnostics(self):
        if self.failure_recorded:return
        self.failure_recorded=True
        try:
            self.remember_child()
            original=self.original_child
            data=dict(original_child=original)
            try:current=self.binding(self.name)
            except Exception as error:
                current={};data['binding_error']=str(error)[:1024]
            same=bool(original) and all(current.get(key)==value for key,value in original.items())
            data['binding_matches']=same
            if same:
                data['exit']={key:current.get(key) for key in ('exited_at','exit_code')}
                if current.get('exited_at'):
                    phase='before theme' if not (self.root/'first-native-theme.txt').exists() else 'after theme'
                    data['summary']=f"native exited {current.get('exit_code')} {phase}"
            # One bounded read of the last eight actual product captures.
            captures=[]
            path=self.root/'theme-captures.json'
            try:
                if path.exists():
                    with path.open('rb') as f:raw=f.read(65537)
                    if len(raw)<=65536:captures=json.loads(raw)[-8:]
            except Exception as error:data['capture_log_error']=str(error)[:1024]
            # Only this fixture's socket and original pane. Native stdout and
            # stderr share its PTY: retain their combined tail, never redirect
            # either native descriptor or interpose a different child process.
            files={}
            if same:
                for name,args in [('native-stderr-tail.txt',['-S','-64']),('dead-private-pane.txt',[])]:
                    try:files[name]=self.capture([*args,'-t',original['pane']],timeout=2).stdout[-16384:]
                    except Exception:pass  # capture() retains the failed read in the ring.
            data['captures']=sorted(captures+list(self.test_captures),key=lambda row:row['started'])[-8:]
            data['native_stderr_source']='combined stdout/stderr from original private PTY'
            retained=os.environ.get('HGS_CLAUDE_THEME_EVIDENCE')
            target=(Path(retained) if retained else test_pause.REPO/'artifacts/test-results/python/claude-theme')/self._testMethodName
            target.mkdir(parents=True,exist_ok=True)
            for name,raw in files.items():(target/name).write_text(raw)
            (target/'failure.json').write_text(json.dumps(data,indent=2)+'\n')
            print('ZERUS_NATIVE_THEME_FAILURE='+json.dumps(data),flush=True)
        except Exception as error:
            # Diagnostics must never replace a timeout/assertion/launch error.
            try:print('ZERUS_NATIVE_THEME_FAILURE='+json.dumps({'diagnostics_error':str(error)[:1024]}),flush=True)
            except Exception:pass

    def tearDown(self):
        try:
            result=self._outcome.result
            if any(case is self for case,_ in result.failures+result.errors):
                self.failure_diagnostics()
            evidence={}
            for name in ('first-native-theme.txt','last-native-theme.txt','theme-keys.jsonl'):
                path=self.root/name
                if path.exists():
                    raw=path.read_bytes()
                    self.assertLessEqual(len(raw),32768)
                    evidence[name]={'sha256':hashlib.sha256(raw).hexdigest(),'raw':raw.decode(errors='replace')}
            evidence['final-screen']={'raw':self.screen()[:16384]}
            print('ZERUS_NATIVE_THEME_EVIDENCE='+json.dumps({'test':self.id(),**evidence}),flush=True)
            retained=os.environ.get('HGS_CLAUDE_THEME_EVIDENCE')
            if retained:
                target=Path(retained)/self._testMethodName
                target.mkdir(parents=True,exist_ok=True)
                for name,item in evidence.items():
                    (target/name).write_text(item['raw'])
                (target/'evidence.json').write_text(json.dumps(evidence,indent=2)+'\n')
        finally:
            # Existing exact private-server process cleanup completes before its
            # fixture directory is removed; no resident server/agent is touched.
            super().tearDown()

    def wait_native(self, predicate, what, timeout=30):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            value=predicate()
            self.remember_child()
            if value:return value
            time.sleep(.025)
        final=self.screen()
        self.failure_diagnostics()
        self.fail('Timed out waiting for '+what+'\n'+final)

    def capture(self, args, timeout=5):
        started=time.monotonic()
        result=None
        error=''
        try:
            result=subprocess.run([test_pause.TMUX,'-S',self.socket,'capture-pane','-p',*args],
                                  env=self.env,capture_output=True,text=True,timeout=timeout)
            return result
        except Exception as caught:
            error=str(caught)[:1024]
            raise
        finally:
            self.test_captures.append(dict(source='test',started=started,ended=time.monotonic(),
                                           returncode=result.returncode if result else None,
                                           stderr=result.stderr[-1024:] if result else error))

    def screen(self):
        record=self.binding(self.name)
        if not record.get('pane'):return ''
        return self.capture(['-t',record['pane']]).stdout

    def keys(self):
        path=self.root/'theme-keys.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def claims(self):
        return [json.loads(path.read_text()) for path in (self.root/'state/claude-theme-confirmations').glob('*.json')
                if not path.name.endswith('.result.json')]

    def launch(self, theme, width, height, hold=False):
        # A legitimate preference chooses the native default; onboarding and
        # every trust/login/permission field remain absent.
        profile={'theme':theme}
        assert not any(key in profile for key in ('hasCompletedOnboarding','hasTrustDialogAccepted','bypassPermissionsModeAccepted'))
        configuration=Path(self.env['CLAUDE_CONFIG_DIR'])
        configuration.mkdir(parents=True,exist_ok=True)
        (configuration/'.claude.json').write_text(json.dumps(profile)+'\n')
        if hold:(self.root/'hold-first-theme').touch()
        # Start a private empty server with its genuine default-size setting.
        # The product still creates and launches the actual agent session.
        tmux_config=self.root/'theme-tmux.conf'
        # Preserve a failed child's terminal until failure-only collection and
        # the existing exact private-server cleanup; never retain a live server.
        tmux_config.write_text(f'set -g exit-empty off\nset -g remain-on-exit on\nset -g default-size {width}x{height}\n')
        subprocess.run([test_pause.TMUX,'-S',self.socket,'-f',str(tmux_config),'start-server'],
                       env=self.env,capture_output=True,check=True,timeout=5)
        self.hgs('claude','p','-n','theme','-d')
        self.wait_native(lambda:(self.root/'first-native-theme.txt').exists(),'the actual first native theme frame')
        record=self.binding(self.name)
        dimensions=subprocess.check_output([test_pause.TMUX,'-S',self.socket,'display-message','-p','-t',record['pane'],
                                           '#{pane_width}\t#{pane_height}'],env=self.env,text=True,timeout=5).strip()
        self.assertEqual(dimensions,f'{width}\t{height}')
        first=(self.root/'first-native-theme.txt').read_text()
        self.assertRegex(first,r'(?m)^\s*Welcome to Claude Code v\d+\.\d+\.\d+\s*$')
        return record,first

    def confirm(self,theme,label,width,height):
        record,first=self.launch(theme,width,height)
        # Genuine native focus/current-value evidence. No replacement or fake
        # capture can manufacture an Auto positive.
        self.assertRegex(first,r'(?m)^\s*❯\s*✔\s*'+re.escape(label)+r'\s*$')
        self.wait_native(lambda:'Select login method:' in self.screen(),'the next manual native login chooser')
        claims=self.claims()
        self.assertEqual(len(claims),1)
        claim=claims[0]
        self.assertEqual(claim['label'],label)
        for field in ('session_id','run_id','pid','process_start','pane'):
            self.assertEqual(claim[field],record[field])
        self.assertEqual(self.keys(),[['send-keys','-t',record['pane'],'Enter']])
        # No automated input is allowed on the subsequent login/terms/trust.
        start=time.monotonic()
        while time.monotonic()-start<5:
            self.assertIn('Select login method:',self.screen())
            self.assertEqual(self.keys(),[['send-keys','-t',record['pane'],'Enter']])
            time.sleep(.25)
        self.assertFalse(self.binding(self.name).get('conversation_id'))
        cards=json.loads(self.hgs('inspect',self.name))['pending_questions']
        self.assertTrue(cards)
        self.assertTrue(all(not card.get('can_answer') for card in cards))

    def manual(self,theme,label,width,height):
        _,first=self.launch(theme,width,height)
        self.assertRegex(first,r'(?m)^\s*❯\s*✔\s*'+re.escape(label)+r'\s*$')
        time.sleep(.5)
        self.assertEqual(self.keys(),[])
        self.assertEqual(self.claims(),[])
        cards=json.loads(self.hgs('inspect',self.name))['pending_questions']
        self.assertTrue(cards)
        card=next(card for card in cards if card.get('tool_call_id')=='claude_theme')
        self.assertFalse(card['can_answer'])
        self.assertEqual(card['answer_transport'],'')
        self.assertIn(label,card['questions'][0]['body'])
        self.assertIn(TITLE,self.screen())
        self.assertEqual(self.keys(),[])

    def test_dark_default_80_by_24(self):self.confirm('dark','Dark mode',80,24)
    def test_dark_default_99_by_54(self):self.confirm('dark','Dark mode',99,54)
    def test_auto_default_80_by_24(self):self.confirm('auto','Auto (match terminal)',80,24)
    def test_auto_default_99_by_54(self):self.confirm('auto','Auto (match terminal)',99,54)
    def test_light_stays_manual_80_by_24(self):self.manual('light','Light mode',80,24)
    def test_light_stays_manual_99_by_54(self):self.manual('light','Light mode',99,54)
    def test_ansi_stays_manual_80_by_24(self):self.manual('dark-ansi','Dark mode (ANSI colors only)',80,24)
    def test_ansi_stays_manual_99_by_54(self):self.manual('dark-ansi','Dark mode (ANSI colors only)',99,54)

    def test_a_real_frame_change_before_guard_never_confirms_the_new_choice(self):
        record,first=self.launch('dark',80,24,hold=True)
        try:
            self.assertRegex(first,r'(?m)^\s*❯\s*✔\s*Dark mode\s*$')
            # Private user input changes only focus; this call bypasses the
            # product observer/log, so keys() counts only actual Zerus input.
            subprocess.run([test_pause.TMUX,'-S',self.socket,'send-keys','-t',record['pane'],'Down'],
                           env=self.env,capture_output=True,check=True,timeout=5)
            self.wait_native(lambda:re.search(r'(?m)^\s*❯\s+(?:✔\s*)?Light mode\s*$',self.screen()),'the genuinely changed native focus')
        finally:(self.root/'hold-first-theme').unlink(missing_ok=True)
        self.wait_native(lambda:'❯' in (self.root/'last-native-theme.txt').read_text()
                         and re.search(r'(?m)^\s*❯\s+(?:✔\s*)?Light mode\s*$',(self.root/'last-native-theme.txt').read_text()),
                         'the guarded helper observing the actual changed frame')
        time.sleep(.3)
        self.assertEqual(self.keys(),[])
        self.assertEqual(self.claims(),[])


if __name__=='__main__':unittest.main()
