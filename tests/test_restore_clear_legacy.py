"""Opt-in mixed hgs versions, with a real private supervisor and C stand-in.

The explicit Clear/reset cut is seeded; the hook and supervisor are full hgs
executables. This does not exercise an old agent release or any owner session.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import unittest
import uuid

import test_clear_context
import test_input


@unittest.skipUnless(os.environ.get('HGS_TEST_LEGACY_HGS'), 'needs a sealed pre-fix hgs artifact')
class LegacyRestoreClear(unittest.TestCase):
    script = test_input.InputTransport.script
    write_record = test_input.InputTransport.write_record
    tmux = test_input.InputTransport.tmux
    wait_for = test_input.InputTransport.wait_for
    received = test_input.InputTransport.received
    tearDown = test_input.InputTransport.tearDown
    usage_history = test_clear_context.ClearContext.usage_history

    def setUp(self):
        self.old = Path(os.environ['HGS_TEST_LEGACY_HGS'])
        self.assertEqual(hashlib.sha256(self.old.read_bytes()).hexdigest(), os.environ['HGS_TEST_LEGACY_SHA256'])
        test_input.InputTransport.setUp(self)
        # Replace only the setup's own placeholder, before starting the target
        # supervisor/child pair. The old supervisor exits normally through its
        # compiled-in finish(), rather than being signalled by the test.
        self.tmux('new-session', '-d', '-s', 'fixture-setup', '/bin/sh')
        self.tmux('set-option', '-g', 'remain-on-exit', 'on')
        self.tmux('kill-session', '-t', self.name)
        (self.root/'agent-pid').unlink()
        self.rollout = self.root/'saved.jsonl'
        self.rollout.write_text(json.dumps(dict(type='session_meta', payload=dict(id=self.conversation_id)))+'\n')
        source = test_clear_context.RESUMED_CODEX.replace(
            'if (argc != 3 ||', 'if (argc == 4 && !strcmp(argv[3], "--no-daemon")) { --argc; }\n    if (argc != 3 ||')
        source = source.replace('if (!strcmp(text, "/clear")', 'if (!strcmp(text, "exit-zero")) return 0;\n                if (!strcmp(text, "exit-fixture")) return 7;\n                if (!strcmp(text, "/clear")')
        (self.root/'codex.c').write_text(source)
        native = self.bin/'codex'
        subprocess.run(['cc', str(self.root/'codex.c'), '-o', str(native)], check=True, capture_output=True, timeout=20)
        self.archive_id = str(uuid.uuid4())
        self.archive_run = str(uuid.uuid4())
        self.archive_path = self.state/'archives'/(self.archive_id+'.json')
        self.archive_path.parent.mkdir(exist_ok=True)
        self.record.update(base=[str(native)], launch_dir=str(self.root), transcript=str(self.rollout))
        archived = dict(self.record, run_id=self.archive_run, archive_id=self.archive_id, state='archived')
        self.archive_path.write_text(json.dumps(archived)+'\n')
        self.archive_bytes = self.archive_path.read_bytes()
        self.write_record()
        env = dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id,
                   HGS_AGENT='codex', HGS_EXPECTED_ID=self.conversation_id,
                   HGS_ARCHIVE_ID=self.archive_id, HGS_EXECUTABLE=str(self.old),
                   CODEX_HOME=str(self.root/'.codex'))
        argv = [str(self.old), '__state', 'run', '0', str(native), str(self.root), str(self.rollout)]
        # A shell exec makes the launched supervisor the pane's original process.
        command = shlex.join(['env', '-i', *[k+'='+v for k,v in env.items()]]) + ' TMUX_PANE="$TMUX_PANE" TMUX="$TMUX" ' + shlex.join(argv) + ' 2>' + shlex.quote(str(self.root/'launch-error'))
        self.tmux('new-session', '-d', '-s', self.name, '-x', '100', '-y', '30', command)
        self.pane = self.tmux('display-message', '-p', '-t', '='+self.name+':', '#{pane_id}').strip()
        self.tmux('set-option', '-t', '='+self.name+':', 'remain-on-exit', 'on')
        self.tmux('set-option', '-t', '='+self.name+':', '@hgs_run', self.run_id)
        try:
            self.wait_for(lambda: (self.root/'agent-pid').exists() and (self.root/'agent-pid').read_text().strip())
        except BaseException:
            print(self.tmux('capture-pane', '-p', '-t', self.pane), flush=True)
            print((self.root/'launch-error').read_text(),flush=True)
            raise
        self.record = json.loads(self.record_path.read_text())
        self.assertEqual(self.record['expected_id'], self.conversation_id)
        self.assertEqual(self.record['restore_archive_id'], self.archive_id)
        supervisor = self.record['supervisor']['pid']
        self.assertEqual(hashlib.sha256(Path('/proc/%d/exe' % supervisor).read_bytes()).hexdigest(), os.environ['HGS_TEST_LEGACY_SHA256'])
        print('mixed inputs', json.dumps(dict(old_sha256=os.environ['HGS_TEST_LEGACY_SHA256'], writer_sha256=hashlib.sha256(test_input.HGS.read_bytes()).hexdigest(), supervisor=supervisor, argv=argv)), flush=True)

    def test_committed_clear_survives_old_supervisor_after_unlink_recovers(self):
        self.committed_exit(False)

    def test_old_clean_exit_archives_successor_with_its_unfinished_identity(self):
        self.committed_exit(True)

    def committed_exit(self, clean):
        request = str(uuid.uuid4())
        self.record['clear_context_request'] = dict(request_id=request, run_id=self.run_id,
            conversation_id=self.conversation_id, restore_archive_id=self.archive_id,
            restore_archive_run_id=self.archive_run)
        self.record['session_clear'] = dict(at=1.0, run_id=self.run_id, conversation_id=self.conversation_id,
            previous_conversation_id=self.conversation_id, source='native_terminal', awaiting_session_start=True)
        self.write_record()
        new = str(uuid.uuid4())
        event = dict(hook_event_name='SessionStart', session_id=new, source='clear',
                     cwd=str(self.root), transcript_path=self.usage_history(new,32,8))
        # Seed the exact durable source-fence cut, after hook acceptance but
        # before successor publication. The full new hook validates and syncs
        # this fence, then attempts the real unlink under EACCES. The ordinary
        # real-Codex positive fixtures exercise writing a previously absent fence.
        archived=json.loads(self.archive_bytes)
        archived['restore_archive_clear_source']=dict(phase='committed',run_id=self.run_id,
            conversation_id=new,previous_conversation_id=self.conversation_id,
            archive_id=self.archive_id,archive_run_id=self.archive_run,request_id=request)
        self.archive_path.write_text(json.dumps(archived)+'\n')
        self.archive_bytes=self.archive_path.read_bytes()
        self.archive_path.parent.chmod(0o500)
        try:
            result = subprocess.run([str(test_input.HGS), '__state', 'hook'],
                env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=self.run_id),
                input=json.dumps(event),text=True,capture_output=True,timeout=12)
            self.assertNotEqual(result.returncode,0,result.stdout)
            self.assertIn('Permission denied',result.stderr)
            committed = json.loads(self.record_path.read_text())
            self.assertEqual(committed['conversation_id'],new)
            self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
            print('committed cut',json.dumps(committed,sort_keys=True),flush=True)
            if 'restore_archive_cleanup' in committed:
                old_view=subprocess.run([str(self.old),'inspect',self.name],env=self.env,text=True,capture_output=True,timeout=12)
                self.assertEqual(old_view.returncode,0,old_view.stderr)
                self.assertFalse(json.loads(old_view.stdout)['clear_context_supported'])
                if not os.environ.get('HGS_TEST_CURRENT_FINALIZER'):
                    self.assertIn('requested context clear',json.loads(old_view.stdout)['clear_context_reason'])
        finally:
            self.archive_path.parent.chmod(0o700)
        if clean:
            committed['session_end'] = dict(run_id=self.run_id,conversation_id=new,reason='other')
            committed['conversation_state'] = 'ended'
            self.record=committed;self.write_record()
        self.tmux('send-keys','-t',self.pane,'-l','exit-zero' if clean else 'exit-fixture')
        self.tmux('send-keys','-t',self.pane,'Enter')
        self.wait_for(lambda:self.tmux('display-message','-p','-t',self.pane,'#{pane_dead}:#{pane_dead_status}').strip()==('1:0' if clean else '1:7'))
        print('supervisor actual pane terminal; active_exists=%s archive=%s' % (self.record_path.exists(),self.archive_path.read_text() if self.archive_path.exists() else '<settled>'),flush=True)
        if clean:
            self.assertFalse(self.record_path.exists())
            successors=[json.loads(p.read_text()) for p in self.archive_path.parent.glob('*.json')
                        if json.loads(p.read_text()).get('conversation_id')==new]
            self.assertEqual(len(successors),1)
            current=successors[0]
        else:
            self.assertTrue(self.record_path.exists(),'old supervisor discarded the committed successor')
            current=json.loads(self.record_path.read_text())
        if os.environ.get('HGS_TEST_CURRENT_FINALIZER'):
            self.assertFalse(self.archive_path.exists())
            self.assertNotIn('restore_archive_cleanup',current)
            self.assertNotIn('clear_context_request',current)
            return
        self.assertEqual((current['run_id'],current['conversation_id']),(self.run_id,new))
        self.assertEqual(current['restore_archive_cleanup']['request_id'],request)
        self.assertEqual(current['clear_context_request']['request_id'],request)
        self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)

        # Current readers cannot replace a stopped unfinished record, including
        # either one-sided marker. An old clean exit may have moved it intact
        # to Archive; current Restore must refuse that same unresolved source.
        if clean:
            before={p:p.read_bytes() for p in self.archive_path.parent.glob('*.json')}
            # Both the successor AND its ORIGINAL source must remain fenced
            # when the old clean finalizer has removed the active binding.
            page=subprocess.run([str(test_input.HGS),'archives','--json'],env=self.env,
                input=json.dumps(dict(limit=50)),text=True,capture_output=True,timeout=12)
            self.assertEqual(page.returncode,0,page.stderr)
            rows=json.loads(page.stdout)['sessions']
            for target in [current['archive_id'],self.archive_id]:
                row=next(row for row in rows if row['archive_id']==target)
                self.assertNotIn('restore',row['allowed_actions'])
                self.assertNotIn('forget',row['allowed_actions'])
                for command in ['resume','kill']:
                    cmd=[str(test_input.HGS),command,self.name,'--archive',target, '--dry-run' if command=='kill' else '-d']
                    result=subprocess.run(cmd,env=self.env,text=True,capture_output=True,timeout=12)
                    self.assertNotEqual(result.returncode,0)
                    self.assertIn('cleanup is unfinished',result.stderr)
                    self.assertEqual(before,{p:p.read_bytes() for p in before})
            return
        self.tmux('kill-session','-t',self.name)  # Both original processes already exited.
        for missing in (None,'restore_archive_cleanup','request_marker'):
            with self.subTest(missing=missing):
                malformed=json.loads(json.dumps(current))
                if missing=='request_marker':
                    malformed['clear_context_request'].pop('restore_archive_cleanup_pending')
                elif missing:
                    malformed.pop(missing)
                self.record=malformed;self.write_record()
                before=self.record_path.read_bytes()
                shown=subprocess.run([str(test_input.HGS),'inspect',self.name],env=self.env,text=True,capture_output=True,timeout=12)
                self.assertEqual(shown.returncode,0,shown.stderr)
                allowed=json.loads(shown.stdout)['allowed_actions']
                for action in ['resume','archive','forget']:
                    self.assertNotIn(action,allowed)
                for option in ([],['--fresh']):
                    cmd=[str(test_input.HGS),'resume',self.name,*option,'-d']
                    result=subprocess.run(cmd,env=self.env,text=True,capture_output=True,timeout=12)
                    self.assertNotEqual(result.returncode,0,result.stdout)
                    self.assertIn('cleanup is unfinished',result.stderr)
                    self.assertEqual(self.record_path.read_bytes(),before)
