"""Passive desktop-v1 kernel through real CLI processes in private state.

No system manager, native agent, production state or tmux server is used.
"""
from contextlib import closing
import errno
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid

HGS = Path(os.environ.get('HGS_TEST_BIN', Path(__file__).resolve().parents[1] / 'target/debug/hgs')).resolve()


def current_store_schema():
    """Read the selected Rust constant, including its standalone leaf alias."""
    desktop = Path(__file__).resolve().parents[1] / 'src/desktop'

    def expression(path):
        match = re.search(r'\bconst\s+SCHEMA\s*:\s*i64\s*=\s*([^;]+);', path.read_text(encoding='utf-8'))
        if match is None:
            raise ValueError(f'No Store schema constant in {path.name}')
        return match[1].strip()

    selected = expression(desktop / 'store.rs')
    if not selected.isdecimal():
        leaf = re.fullmatch(r'super::([a-z][a-z_0-9]*)::SCHEMA', selected)
        if leaf is None:
            raise ValueError('Unsupported Store schema expression; update the fixture reader')
        selected = expression(desktop / (leaf[1] + '.rs'))
    return int(selected)


SCHEMA = current_store_schema()


class Desktop(unittest.TestCase):
    @unittest.skipUnless(sys.platform.startswith(('linux', 'darwin')), 'Unix private foreground holder')
    def test_installer_retains_the_real_original_hold_through_private_passive_restart(self):
        """Real holder in isolated non-agent CI/private foreground ancestry."""
        if sys.platform == 'darwin':
            from macos_ancestry import topology, unsupported_hosted_foreign
            chain = topology()
            print('MACOS_HOLD_TOPOLOGY ' + json.dumps(chain), file=sys.stderr, flush=True)
            unsupported = unsupported_hosted_foreign(chain, os.geteuid(), os.environ)
            if unsupported is not None:
                self.skipTest('Hosted runner has unsupported foreign ancestor: ' + json.dumps(unsupported)
                              + '; native macOS permission remains refused')
        import pty
        service, _ = self.serve()
        self.stop(service)
        # A pre-existing private native-host lock is also retained by Root.
        fd = os.open(self.desktop / 'notification-host.lock', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)
        request = str(uuid.uuid4())
        receipt = self.cli('desktop', 'revoke', '--json', '--request-id', request, '--reason', 'rollback')
        protocol = Path(__file__).resolve().parents[1] / 'scripts/desktop-revoke-hold.py'
        script = r'''import fcntl,importlib.util,json,os,signal,subprocess,sys,termios,time
tty=os.open(sys.argv[1],os.O_RDWR)
fcntl.ioctl(tty,termios.TIOCSCTTY,0)
old=signal.signal(signal.SIGTTOU,signal.SIG_IGN)
os.tcsetpgrp(tty,os.getpgrp());signal.signal(signal.SIGTTOU,old)
spec=importlib.util.spec_from_file_location('hold',sys.argv[2]);module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
hgs=sys.argv[3];receipt=json.loads(sys.argv[4]);locks=json.loads(sys.argv[5])
help_text=subprocess.check_output([hgs,'--help'],timeout=5).decode()
hold=module.Hold(hgs,os.environ,receipt['configuration'],receipt['request_id'],receipt,help_text).acquire()
service=None
try:
 for path in locks:
  with open(path,'rb') as lock:
   try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
   except BlockingIOError:pass
   else:raise AssertionError('original native exclusion is not held')
 service=subprocess.Popen([hgs,'desktop','serve'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,close_fds=True)
 until=time.monotonic()+8
 while True:
  result=subprocess.run([hgs,'desktop','status','--json'],capture_output=True,timeout=2)
  if result.returncode==0 and json.loads(result.stdout).get('lifecycle',{}).get('ipc_ready'):break
  assert service.poll() is None and time.monotonic()<until
  time.sleep(.02)
 hold.check()
 service.terminate();_,error=service.communicate(timeout=5);assert len(error)<=65536
 hold.check();hold.close()
 assert hold.child.returncode==0 and hold.finished
 for path in locks:
  with open(path,'rb') as lock:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
 print(json.dumps(dict(passive_ready_while_held=True,original_holder_reaped=True,pipes_drained=True)))
finally:
 if service is not None and service.poll() is None:
  service.terminate();service.communicate(timeout=5)
 hold.close(validate=False);os.close(tty)
'''
        master, slave = pty.openpty()
        try:
            locks = [str(self.desktop / name) for name in ('handoff.lock', 'notification-host.lock')]
            result = subprocess.run([sys.executable, '-c', script, os.ttyname(slave), str(protocol),
                                     str(HGS), json.dumps(receipt), json.dumps(locks)],
                                    env=self.env, start_new_session=True, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=25)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads(result.stdout), dict(passive_ready_while_held=True,
                             original_holder_reaped=True, pipes_drained=True))
        finally:
            os.close(master)
            os.close(slave)

    @unittest.skipUnless(sys.platform.startswith(('linux', 'darwin', 'win32')), 'Native rollback hold')
    def test_rollback_hold_agent_refusal_is_structured_and_creates_no_state(self):
        selected = dict(self.env, HGS_SESSION='private-agent-fixture')
        result = subprocess.run([str(HGS), 'desktop', 'revoke-hold', '--json', '--request-id', str(uuid.uuid4())],
                                input=b'', stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                env=selected, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        value = json.loads(result.stdout)
        self.assertEqual(value['abi'], 'desktop-revoke-hold-v1')
        self.assertIs(value['held'], False)
        self.assertEqual(value['code'], 'revoke_hold_refused')
        self.assertFalse(self.desktop.exists())

    def test_notification_controls_are_scoped_reads_and_generic_mutation_is_refused(self):
        self.serve()
        current = self.cli('desktop', 'status', '--json')
        scope = dict(configuration=current['configuration'], service_epoch=current['epoch'])
        query = dict(op='notification_preferences_status', request_id=str(uuid.uuid4()), scope=scope)
        status = self.cli('desktop', 'request', '--json', data=query)
        self.assertEqual(status['abi'], 'desktop-notification-controls-v1')
        self.assertEqual(status['scope'], scope)
        self.assertFalse(status['configured'])
        self.assertEqual(status['preferences'], dict(optional_questions=False))
        original = dict(kind='configure', request_id=str(uuid.uuid4()), journal=status['journal'],
                        revision=0, preferences=dict(optional_questions=True))
        mutation = dict(op='notification_control', scope=scope, control=original)
        refused = self.cli('desktop', 'request', '--json', data=mutation)
        self.assertFalse(refused['accepted'])
        self.assertEqual(refused['code'], 'frontend_authentication_required')
        lookup = dict(op='notification_preferences_result', query=dict(
            request_id=str(uuid.uuid4()), scope=scope, original=original))
        result = self.cli('desktop', 'request', '--json', data=lookup)
        self.assertTrue(result['definitive_not_applied'])
        self.assertIsNone(result['receipt'])
        bad = dict(query, scope=dict(scope, service_epoch=str(uuid.uuid4())))
        self.assertEqual(self.cli('desktop', 'request', '--json', data=bad)['code'], 'notification_control_query_refused')
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_notification_preferences_requests').fetchone()[0], 0)
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_attempts').fetchone()[0], 0)

    def test_creation_clone_is_a_closed_native_intent_with_scoped_read_only_lookup(self):
        self.serve()
        status = self.cli('desktop', 'status', '--json')
        scope = dict(configuration=status['configuration'], service_epoch=status['epoch'])
        creation, clone = str(uuid.uuid4()), str(uuid.uuid4())
        original = dict(id=creation, creation_schema=1, created=1, revision=4, cmd='codex', tag='example',
                        host='', state='cloning', repository=True, parent='/srv', target='/srv/repo',
                        repository_name='repo', url='https://example.invalid/repo.git', clone_id=clone,
                        expected_machine='22222222-2222-4222-8222-222222222222')
        intent = dict(scope=scope, creation_id=creation, clone_request_id=clone, revision=4,
                      original=json.dumps(original, separators=(',', ':')))
        refused = self.cli('desktop', 'request', '--json', data=dict(op='creation_clone', intent=intent))
        self.assertFalse(refused['accepted'])
        self.assertEqual(refused['code'], 'frontend_authentication_required')
        query = dict(request_id=str(uuid.uuid4()), scope=scope, creation_id=creation, clone_request_id=clone,
                     plan_sha256='a' * 64)
        result = self.cli('desktop', 'request', '--json', data=dict(op='creation_clone_result', query=query))
        self.assertEqual(result['abi'], 'desktop-creation-clone-v1')
        self.assertEqual(result['scope'], scope)
        self.assertTrue(result['read_only'])
        self.assertFalse(result['found'])
        self.assertIsNone(result['receipt'])
        bad = dict(query, scope=dict(scope, service_epoch=str(uuid.uuid4())))
        self.assertEqual(self.cli('desktop', 'request', '--json', data=dict(op='creation_clone_result', query=bad))['code'], 'creation_result_unavailable')
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_creation_clones').fetchone()[0], 0)
            self.assertEqual(db.execute('SELECT rows,bytes,active FROM desktop_creation_clone_budget').fetchone(), (0, 0, 0))
        self.assertEqual(status['operation_queue']['kinds']['creation_clone']['undrained'], 0)

    @unittest.skipUnless(sys.platform.startswith('linux') or sys.platform == 'darwin', 'Unix registration mode')
    def test_registration_variant_is_read_only_and_service_reports_its_requested_mode(self):
        passive = self.cli('desktop', 'registration', '--json')
        background = self.cli('desktop', 'registration', '--json', '--background')
        self.assertEqual(passive['mode'], 'passive')
        self.assertEqual(background['mode'], 'background')
        self.assertEqual(background['argv'], (passive['argv'][:-1] + ['--background', '--launchd'])
                         if sys.platform == 'darwin' else passive['argv'] + ['--background'])
        self.assertEqual(background['configuration'], passive['configuration'])
        self.assertFalse(self.desktop.exists())
        process, status = self.serve()
        self.assertEqual(status['lifecycle']['mode'], 'passive')
        self.stop(process)
        process, status = self.serve(background=True)
        self.assertEqual(status['lifecycle']['mode'], 'background')
        self.stop(process)

    @unittest.skipUnless(sys.platform.startswith('linux') or sys.platform == 'darwin', 'Unix native caller boundary')
    def test_mode_caller_requires_foreground_tty_or_the_actual_installed_frontend_and_refuses_agent_ancestry(self):
        """Run in isolated CI/private runner, outside any native agent ancestry.

        The positive TTY case deliberately refuses inside an agent tree even
        when child markers are cleared; a private pty is not a bypass.
        """
        unsupported = None
        if sys.platform == 'darwin':
            from macos_ancestry import topology, unsupported_hosted_foreign
            chain = topology()
            print('MACOS_CALLER_TOPOLOGY ' + json.dumps(chain), file=sys.stderr, flush=True)
            unsupported = unsupported_hosted_foreign(chain, os.geteuid(), os.environ)
        import shutil
        # This helper is a private fixture executable, not the owner's GUI.
        frontend = Path(self.env['HOME']) / '.local/bin/hgs-tray'
        frontend.parent.mkdir(parents=True, exist_ok=True)
        # A native fixture avoids relocating the hosted Mac Python framework.
        source = r"""#include <unistd.h>
#include <sys/wait.h>
#include <signal.h>
#include <time.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc != 2) return 90;
    pid_t child = fork();
    if (child < 0) return 91;
    if (child == 0) {
        execl(argv[1], argv[1], "desktop", "registration", "mode-check", "--json", (char *)0);
        _exit(92);
    }
    int status;
    time_t until = time(NULL) + 5;
    while (time(NULL) < until) {
        pid_t result = waitpid(child, &status, WNOHANG);
        if (result == child) return WIFEXITED(status) ? WEXITSTATUS(status) : 93;
        if (result < 0) return 94;
        usleep(10000);
    }
    kill(child, SIGKILL); /* The original unreaped fixture child only. */
    waitpid(child, &status, 0);
    return 95;
}
"""
        compiled = subprocess.run(['cc', '-x', 'c', '-', '-o', str(frontend)], input=source,
                                  env=self.env, capture_output=True, text=True, timeout=20)
        self.assertEqual(compiled.returncode, 0, compiled.stderr)
        script = '''import os,sys,json,subprocess,fcntl,termios,signal
tty=None
try:
 if sys.argv[1]!='none':
  tty=os.open(sys.argv[1],os.O_RDWR)
  fcntl.ioctl(tty,termios.TIOCSCTTY,0)
  old=signal.signal(signal.SIGTTOU,signal.SIG_IGN)
  try: os.tcsetpgrp(tty,os.getpgrp())
  finally: signal.signal(signal.SIGTTOU,old)
 env=dict(os.environ)
 if sys.argv[2]=='stripped':
  env.pop('HGS_SESSION',None);env.pop('HGS_RUN_ID',None)
 child=subprocess.run([sys.argv[3],'desktop','registration','mode-check','--json'],env=env,
                      stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=5)
 print(json.dumps(dict(code=child.returncode,out=child.stdout.decode(),error=child.stderr.decode())))
finally:
 if tty is not None: os.close(tty)
'''
        def case(image, terminal=False, marker=False, native=False):
            import pty
            env = dict(self.env)
            if marker:
                env['HGS_SESSION'] = 'private-agent-fixture'
            master = slave = None
            try:
                if terminal:
                    master, slave = pty.openpty()
                args = [str(image), str(HGS)] if native else [str(image), '-c', script,
                    os.ttyname(slave) if terminal else 'none', 'stripped', str(HGS)]
                child = subprocess.run(args,
                    env=env, start_new_session=True, stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=8, text=True)
                if native:
                    return dict(code=child.returncode, out=child.stdout, error=child.stderr)
                self.assertEqual(child.returncode, 0, child.stderr)
                return json.loads(child.stdout)
            finally:
                if master is not None:
                    os.close(master); os.close(slave)
        # These refusals must run even on an unsupported hosted topology.
        headless = case(sys.executable)
        self.assertNotEqual(headless['code'], 0)
        agent = case(sys.executable, terminal=True, marker=True)
        self.assertNotEqual(agent['code'], 0)
        self.assertIn('agent-descended', agent['error'])
        named = frontend.with_name('claude')
        shutil.copy2(frontend, named)
        named_agent = case(named, native=True)
        self.assertNotEqual(named_agent['code'], 0)
        self.assertIn('agent-descended', named_agent['error'])
        # The checker itself cannot impersonate its own frontend ancestor.
        native_frontend = frontend.read_bytes()
        try:
            shutil.copy2(HGS, frontend)
            own = subprocess.run([str(frontend), 'desktop', 'registration', 'mode-check', '--json'],
                                 env=self.env, start_new_session=True, stdin=subprocess.DEVNULL,
                                 capture_output=True, text=True, timeout=5)
            self.assertNotEqual(own.returncode, 0)
        finally:
            frontend.write_bytes(native_frontend)
        self.assertFalse(self.desktop.exists())
        if unsupported is not None:
            print('MACOS_CALLER_REFUSALS_COMPLETED', file=sys.stderr, flush=True)
            self.skipTest('Hosted runner has unsupported foreign ancestor: ' + json.dumps(unsupported)
                          + '; native macOS permission remains refused; refusal cases passed')
        foreground = case(sys.executable, terminal=True)
        self.assertEqual(foreground['code'], 0, foreground)
        self.assertEqual(json.loads(foreground['out'])['source'], 'interactive_terminal')
        gui = case(frontend, native=True)
        self.assertEqual(gui['code'], 0, gui)
        self.assertEqual(json.loads(gui['out'])['source'], 'installed_frontend')

    def test_approval_audit_is_selected_scoped_and_does_not_create_a_journal(self):
        self.serve()
        status = self.cli('desktop', 'status', '--json')
        query = dict(op='approval_audit_list', request_id=str(uuid.uuid4()),
                     configuration=status['configuration'], epoch=status['epoch'])
        reply = self.cli('desktop', 'request', '--json', data=query)
        self.assertEqual(reply['abi'], 'desktop-approval-audit-v1')
        self.assertTrue(reply['accepted'])
        self.assertTrue(reply['read_only'])
        self.assertEqual(reply['configuration'], status['configuration'])
        self.assertEqual(reply['service_epoch'], status['epoch'])
        self.assertEqual(reply['result']['entries'], [])
        self.assertFalse((self.state / 'approval-audit').exists())
        query['epoch'] = str(uuid.uuid4())
        refused = self.cli('desktop', 'request', '--json', data=query)
        self.assertFalse(refused['accepted'])
        self.assertEqual(refused['code'], 'approval_audit_scope_changed')

    def test_ownership_read_preflight_is_independent_and_never_creates_absent_state(self):
        value = self.cli('desktop', 'ownership-status', '--json')
        self.assertEqual(value['abi'], 'desktop-ownership-v1')
        self.assertEqual(value['state'], 'passive')
        self.assertIsNone(value['journal'])
        self.assertEqual(value['revision'], 0)
        self.assertFalse(self.desktop.exists())
        query = dict(op='desktop_ownership_result', request_id=str(uuid.uuid4()),
                     configuration=value['configuration'], original_request_id=str(uuid.uuid4()),
                     fingerprint='a' * 64, journal=None, revision=0)
        result = self.cli('desktop', 'ownership-result', '--json', data=query)
        self.assertFalse(result['found'])
        self.assertFalse(result['definitive'])
        self.assertFalse(self.desktop.exists())

    def test_ownership_preflight_and_lookup_use_current_selected_actor_scope(self):
        process, service = self.serve()
        value = self.cli('desktop', 'ownership-status', '--json')
        self.assertEqual(value['configuration'], service['configuration'])
        self.assertEqual(value['state'], 'passive')
        self.assertEqual(value['revision'], 0)
        self.assertEqual(service['desktop_ownership'], value)
        query = dict(op='desktop_ownership_result', request_id=str(uuid.uuid4()),
                     configuration=value['configuration'], original_request_id=str(uuid.uuid4()),
                     fingerprint='a' * 64, journal=value['journal'], revision=0)
        result = self.cli('desktop', 'ownership-result', '--json', data=query)
        self.assertFalse(result['found'])
        self.assertTrue(result['definitive'])
        bad = dict(query, extra_authority=True)
        refused = self.cli('desktop', 'request', '--json', data=bad)
        self.assertFalse(refused['accepted'])
        self.assertEqual(refused['code'], 'ownership_read_refused')
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), value)
        self.stop(process)
        offline = self.cli('desktop', 'ownership-status', '--json')
        self.assertEqual(offline, value)
        self.assertEqual(self.cli('desktop', 'ownership-result', '--json', data=query), result)

    def test_generic_ownership_mutation_has_no_authority_or_durable_reservation(self):
        self.serve()
        value = self.cli('desktop', 'ownership-status', '--json')
        for action in ('grant', 'rearm', 'native_takeover'):
            request = dict(op='desktop_ownership', request_id=str(uuid.uuid4()), action=action,
                           journal=value['journal'], revision=value['revision'])
            result = self.cli('desktop', 'request', '--json', data=request)
            self.assertFalse(result['accepted'])
            self.assertFalse(result['durable'])
            self.assertEqual(result['code'], 'frontend_authentication_required')
            self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), value)
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_ownership_requests').fetchone()[0], 0)

    def test_schema_fourteen_ownership_migration_is_atomic_and_never_grants(self):
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            self.remove_cache_schema(db)
            db.execute('DROP TABLE desktop_creation_clones')
            db.execute('DROP TABLE desktop_creation_clone_budget')
            db.execute('DROP TABLE desktop_notification_preferences_requests')
            db.execute('DROP TABLE desktop_notification_preferences')
            db.execute('DROP TABLE desktop_ownership_requests')
            db.execute('DROP TABLE desktop_ownership')
            db.execute('PRAGMA user_version=14')
            db.execute('CREATE TABLE desktop_ownership(fixture TEXT)')
            db.execute("INSERT INTO desktop_ownership VALUES('retained')")
        self.failed_serve()
        with closing(sqlite3.connect(database)) as db, db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 14)
            self.assertEqual(db.execute('SELECT fixture FROM desktop_ownership').fetchone()[0], 'retained')
            self.assertEqual(db.execute("SELECT count(*) FROM sqlite_schema WHERE name='desktop_ownership_requests'").fetchone()[0], 0)
            db.execute('DROP TABLE desktop_ownership')
        _, service = self.serve()
        self.assertEqual(service['schema'], SCHEMA)
        self.assertEqual(service['desktop_ownership']['state'], 'passive')
        self.assertFalse(service['writers_enabled'])

    def setUp(self):
        # macOS's per-user TMPDIR (/var/folders/...) leaves too little of the
        # 104-byte socket path for the desktop service under nested state dirs.
        temp = tempfile.TemporaryDirectory(prefix='hgs-desk-', dir='/tmp' if sys.platform == 'darwin' else None)
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.state = self.root / 'state'
        self.desktop = self.state / 'desktop'
        self.config = self.root / 'config'
        self.config.mkdir(mode=0o700)
        private_tmp = self.root / 'tmp'
        private_tmux = self.root / 'tmux'
        private_tmp.mkdir(mode=0o700)
        private_tmux.mkdir(mode=0o700)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('HGS_') and k not in ('TMUX', 'TMUX_PANE')}
        self.env.update(HOME=str(self.root), USERPROFILE=str(self.root),
                        TMPDIR=str(private_tmp), TMUX_TMPDIR=str(private_tmux),
                        HGS_CONFIG_DIR=str(self.config), HGS_STATE_DIR=str(self.state),
                        HGS_SELF='example', HGS_PEERS='peer.example')
        if os.name != 'nt':
            # A passive service must not contact any peer/manager/terminal.
            fake = self.root / '.local/bin'
            fake.mkdir(parents=True)
            for name in ('ssh', 'tmux', 'systemctl', 'launchctl', 'schtasks'):
                program = fake / name
                program.write_text('#!/bin/sh\nprintf called >> "$HOME/unexpected-tool"\nexit 99\n')
                program.chmod(0o700)

    def cli(self, *args, data=None, ok=True, env=None):
        result = subprocess.run([str(HGS), *args], input=None if data is None else json.dumps(data),
                                env=env or self.env, capture_output=True, text=True, timeout=10)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def stop(self, process):
        if process.poll() is None:
            process.terminate()
            output, error = process.communicate(timeout=8)
            if os.name != 'nt':
                self.assertEqual(process.returncode, 0, error + output)

    def serve(self, background=False):
        # The service must create a private endpoint even when its launcher
        # supplies a permissive mask; probing readiness races socket creation.
        options = {'umask': 0} if os.name != 'nt' else {}
        args = [str(HGS), 'desktop', 'serve'] + (['--background'] if background else [])
        process = subprocess.Popen(args, env=self.env,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, **options)
        self.addCleanup(self.stop, process)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if process.poll() is not None:
                self.fail('service failed to start: ' + process.communicate()[1])
            status = self.cli('desktop', 'status', '--json')
            if status['running']:
                return process, status
            time.sleep(0.02)
        self.fail('service did not become ready')

    def failed_serve(self, env=None):
        return self.cli('desktop', 'serve', ok=False, env=env)

    def test_pending_read_import_requires_native_frontend_and_lookup_stays_read_only(self):
        self.serve()
        mark = dict(computer='example', name='agent/project/test', run_id='run',
                    conversation_id='conversation', reply_id='reply', attention_signature=None)
        page = dict(source=dict(sha256='a' * 64, bytes=256, rows=1), after=0, marks=[mark])
        refused = self.cli('desktop', 'request', '--json', data=dict(
            op='import_pending_reads', request_id=str(uuid.uuid4()), page=page,
            confirmed=True, role='frontend'))
        self.assertFalse(refused['accepted'])
        self.assertFalse(refused['durable'])
        self.assertEqual(refused['code'], 'frontend_authentication_required')
        lookup = self.cli('desktop', 'request', '--json', data=dict(
            op='pending_reads_import_result', request_id=str(uuid.uuid4()),
            original_request_id=str(uuid.uuid4()), page=page))
        self.assertTrue(lookup['accepted'])
        self.assertFalse(lookup['durable'])
        self.assertFalse(lookup['found'])
        self.assertIsNone(lookup['result'])
        status = self.cli('desktop', 'status', '--json')
        self.assertFalse(status['writers_enabled'])
        self.assertFalse(status['automatic_approvals'])
        self.assertEqual(status['read_queue']['pending'], 0)

    def watch(self):
        process = subprocess.Popen([str(HGS), 'desktop', 'watch', '--json'], env=self.env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        frames = queue.Queue(maxsize=32)

        def read():
            for line in process.stdout:
                frames.put(json.loads(line), timeout=5)

        reader = threading.Thread(target=read, daemon=True)
        reader.start()

        def close():
            if not process.stdin.closed:
                process.stdin.close()
            if process.poll() is None:
                process.wait(timeout=5)
            reader.join(timeout=5)
            self.assertFalse(reader.is_alive(), 'watch reader did not drain')
            for file in (process.stdout, process.stderr):
                file.close()

        self.addCleanup(close)
        return process, frames.get(timeout=5), frames

    def startup(self):
        process = subprocess.Popen([str(HGS), 'desktop', 'startup', '--json'], env=self.env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

        def close():
            if process.stdin is not None:
                process.stdin.close()
                process.stdin = None
            if process.poll() is None:
                process.terminate()
            process.communicate(timeout=5)
        self.addCleanup(close)
        return process

    def startup_frame(self, op, nonce, configuration=None):
        return dict(abi='desktop-startup-v1', version=1, op=op, nonce=nonce, configuration=configuration)

    def startup_send(self, process, frame):
        process.stdin.write(json.dumps(frame) + '\n')
        process.stdin.flush()

    def startup_read(self, process):
        import select
        self.assertTrue(select.select([process.stdout], [], [], 3)[0], 'startup frame deadline')
        return json.loads(process.stdout.readline())

    def startup_end(self, process, success):
        process.stdin.close()
        process.stdin = None
        out, error = process.communicate(timeout=3)
        self.assertEqual(process.returncode == 0, success, out + error)
        return out

    @unittest.skipUnless(os.name == 'posix', 'Unix startup exclusion only')
    def test_startup_absence_creates_nothing_and_requires_exact_release_then_exit(self):
        configuration = self.cli('desktop', 'status', '--json')['configuration']
        process = self.startup()
        nonce = str(uuid.uuid4())
        self.startup_send(process, self.startup_frame('hold', nonce))
        self.assertEqual(self.startup_read(process), self.startup_frame('ready', nonce, configuration))
        self.assertFalse(self.state.exists())
        self.assertIsNone(process.poll())
        self.startup_send(process, self.startup_frame('release', nonce, configuration))
        self.assertEqual(self.startup_read(process), self.startup_frame('released', nonce, configuration))
        self.assertEqual(self.startup_end(process, True), '')
        self.assertFalse(self.state.exists())

    @unittest.skipUnless(os.name == 'posix', 'Unix startup exclusion only')
    def test_startup_holds_the_actual_existing_boundary_until_helper_exit(self):
        import fcntl
        service, status = self.serve()
        self.stop(service)
        process = self.startup()
        nonce = str(uuid.uuid4())
        self.startup_send(process, self.startup_frame('hold', nonce))
        ready = self.startup_read(process)
        self.assertEqual(ready['configuration'], status['configuration'])
        with (self.desktop / 'handoff.lock').open('r+b') as lock:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            other = self.startup()
            self.startup_send(other, self.startup_frame('hold', str(uuid.uuid4())))
            self.assertEqual(self.startup_end(other, False), '')
            self.startup_send(process, self.startup_frame('release', nonce, status['configuration']))
            self.assertEqual(self.startup_read(process)['op'], 'released')
            self.startup_end(process, True)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

    @unittest.skipUnless(os.name == 'posix', 'Unix startup exclusion only')
    def test_startup_retained_preparation_and_abort_intent_refuse_without_mutation(self):
        service, status = self.serve()
        self.stop(service)
        configuration = status['configuration']
        with sqlite3.connect(self.desktop / 'state.sqlite3') as db:
            epoch = db.execute('SELECT epoch FROM service').fetchone()[0]
            new_id = lambda: str(uuid.uuid4())
            binding = dict(version=1, request_id=new_id(), configuration=configuration,
                           service_epoch=epoch, producer_epoch=new_id(), writer_epoch=new_id(),
                           drain_id=new_id(), import_id=new_id(), profile_id='g1-background-v1',
                           profile_hash='84ff9e4901c3df222b879922c756dcd364ab04300d5d3c83916320dd250b72ff',
                           frontend=dict(abi='desktop-delegation-v1', version=1, nonce=new_id(),
                                         application='com.hgdev.hgs-tray', gui='tauri',
                                         build=dict(version='1.2.3', source_commit='b' * 40, source_dirty=False),
                                         instance_epoch=new_id(), writer_epoch=None, configuration=configuration,
                                         live=True, replay=False))
            body = json.dumps(binding)
            digest = hashlib.sha256(b'hgs-desktop-prepared-v1\0' + body.encode()).hexdigest()
            db.execute('INSERT INTO desktop_producer_prepared VALUES(1,?,?,?)', (body, digest, 'prepared'))
            db.commit()
            for phase in ('prepared', 'abort_requested'):
                db.execute('UPDATE desktop_producer_prepared SET phase=?', (phase,))
                db.commit()
                before = db.execute('SELECT * FROM desktop_producer_prepared').fetchall()
                process = self.startup()
                self.startup_send(process, self.startup_frame('hold', new_id()))
                self.assertEqual(self.startup_end(process, False), '')
                self.assertEqual(db.execute('SELECT * FROM desktop_producer_prepared').fetchall(), before)

    @unittest.skipUnless(os.name == 'posix', 'Unix startup exclusion only')
    def test_startup_malformed_or_oversized_initial_frames_never_reply_or_create_state(self):
        nonce = str(uuid.uuid4())
        valid = self.startup_frame('hold', nonce)
        for changed in (dict(valid, abi='unknown'), dict(valid, version=2),
                        dict(valid, nonce='invalid'), dict(valid, op='release'),
                        dict(valid, authority=True), dict(valid, configuration='a' * 64)):
            process = self.startup()
            self.startup_send(process, changed)
            self.assertEqual(self.startup_end(process, False), '')
        process = self.startup()
        process.stdin.write('x' * 4097)
        process.stdin.flush()
        self.assertEqual(self.startup_end(process, False), '')
        self.assertFalse(self.state.exists())

    @unittest.skipUnless(os.name == 'posix', 'Unix startup exclusion only')
    def test_startup_foreign_release_or_eof_cannot_mint_completion(self):
        configuration = self.cli('desktop', 'status', '--json')['configuration']
        for changed in ('nonce', 'configuration', 'eof'):
            nonce = str(uuid.uuid4())
            process = self.startup()
            self.startup_send(process, self.startup_frame('hold', nonce))
            self.startup_read(process)
            if changed != 'eof':
                self.startup_send(process, self.startup_frame('release',
                    str(uuid.uuid4()) if changed == 'nonce' else nonce,
                    'b' * 64 if changed == 'configuration' else configuration))
            self.assertEqual(self.startup_end(process, False), '')
        self.assertFalse(self.state.exists())

    @unittest.skipUnless(os.name == 'posix', 'Unix startup exclusion only')
    def test_startup_partial_progress_does_not_renew_its_fixed_deadline(self):
        process = self.startup()
        start = time.monotonic()
        process.stdin.write('{')
        process.stdin.flush()
        with self.assertRaises(subprocess.TimeoutExpired):
            process.wait(timeout=0.05)
        # Keep the input open. The helper itself must expire without EOF/release.
        self.assertNotEqual(process.wait(timeout=11), 0)
        self.assertLess(time.monotonic() - start, 11)
        self.assertEqual(process.stdout.read(), '')
        self.assertFalse(self.state.exists())

    def test_status_is_read_only_and_does_not_start_service(self):
        status = self.cli('desktop', 'status', '--json')
        self.assertFalse(status['running'])
        self.assertFalse(self.state.exists())
        self.cli('desktop', 'watch', '--json', ok=False)
        self.assertFalse(self.state.exists())
        self.cli('@peer.example', 'desktop', 'status', '--json', ok=False)
        self.cli('--dry-run', 'desktop', 'serve', ok=False)
        self.assertFalse((self.root / 'unexpected-tool').exists())

    @unittest.skipUnless(os.name == 'posix', 'Unix endpoint publication ordering')
    def test_status_does_not_connect_to_a_service_waiting_for_its_schema_transaction(self):
        import fcntl
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db:
            # Rebuild the real predecessor so startup must take a write
            # transaction. Hold it until the read-only readiness probe returns.
            db.executescript('DROP TRIGGER desktop_cache_effect_monotone; DROP TRIGGER desktop_cache_kept_original; '
                             'ALTER TABLE desktop_cache_operations DROP COLUMN effect_started; '
                             'ALTER TABLE desktop_cache_operations DROP COLUMN kept_receipt; PRAGMA user_version=18;')
            db.execute('BEGIN IMMEDIATE')
            process = subprocess.Popen([str(HGS), 'desktop', 'serve'], env=self.env,
                                       stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, text=True)
            self.addCleanup(process.stdout.close)
            self.addCleanup(process.stderr.close)
            self.addCleanup(self.stop, process)
            try:
                deadline = time.monotonic() + 3
                with (self.desktop / 'owner.lock').open('r+b') as owner:
                    while True:
                        self.assertIsNone(process.poll(), 'original startup exited before owner acquisition')
                        try:
                            fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        except BlockingIOError:
                            break
                        fcntl.flock(owner, fcntl.LOCK_UN)
                        self.assertLess(time.monotonic(), deadline, 'original startup did not acquire its owner')
                        time.sleep(0.005)
                status = self.cli('desktop', 'status', '--json')
                self.assertFalse(status['running'])
                self.assertIsNone(process.poll())
                self.assertFalse((self.desktop / 'service.sock').exists())
            finally:
                db.rollback()
        deadline = time.monotonic() + 5
        while not self.cli('desktop', 'status', '--json')['running']:
            self.assertIsNone(process.poll(), 'original startup exited after releasing the schema transaction')
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)

    @unittest.skipIf(os.name == 'nt', 'Windows preview requires an installed stable CLI')
    def test_registration_preview_preserves_selected_paths_without_installing(self):
        preview = self.cli('desktop', 'registration', '--json')
        self.assertEqual(preview['operation'], 'preview')
        expected = ['desktop', 'serve', '--config-dir', str(self.config), '--state-dir', str(self.state)]
        if os.uname().sysname == 'Darwin':
            expected.append('--launchd')
        self.assertEqual(preview['argv'][1:], expected)
        self.assertEqual(Path(preview['argv'][0]).resolve(), HGS)
        self.assertEqual(preview['configuration'], self.cli('desktop', 'status', '--json')['configuration'])
        self.assertFalse(Path(preview['destination']).exists())
        self.assertFalse(self.state.exists())
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_launchd_permanent_refusal_stops_restarts_but_ordinary_cli_fails(self):
        args = ['desktop', 'serve', '--config-dir', 'relative', '--state-dir', str(self.state)]
        result = self.cli(*args, ok=False)
        self.assertEqual(result.returncode, 78)
        refused = subprocess.run([str(HGS), *args, '--launchd'], env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(refused.returncode, 0, refused.stderr)
        self.assertIn('automatic restart paused', refused.stderr)
        self.assertFalse(self.state.exists())
        # A private-dir refusal after configuration loading uses the same lane.
        self.state.mkdir()
        (self.state / 'desktop').write_text('foreign data')
        args[3] = str(self.config)
        result = self.cli(*args, ok=False)
        self.assertEqual(result.returncode, 78)
        refused = subprocess.run([str(HGS), *args, '--launchd'], env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(refused.returncode, 0, refused.stderr)
        self.assertIn('desktop startup refused', refused.stderr)
        self.assertEqual((self.state / 'desktop').read_text(), 'foreign data')
        # Configuration loading fails before desktop dispatch and must not loop.
        (self.config / 'config').mkdir()
        refused = subprocess.run([str(HGS), *args, '--launchd'], env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(refused.returncode, 0, refused.stderr)
        self.assertIn('automatic restart paused', refused.stderr)
        # Unrelated/prefixed commands cannot gain the launchd exit mapping.
        self.cli('@example', *args, '--launchd', ok=False)
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_registered_launch_pins_directories_instead_of_inherited_environment(self):
        selected = self.root / 'selected config'
        state = self.root / 'selected state'
        selected.mkdir(mode=0o700)
        process = subprocess.Popen([str(HGS), 'desktop', 'serve', '--config-dir', str(selected), '--state-dir', str(state)], env=self.env,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop, process)
        env = dict(self.env, HGS_CONFIG_DIR=str(selected), HGS_STATE_DIR=str(state))
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            self.assertIsNone(process.poll(), 'registered launch exited')
            if self.cli('desktop', 'status', '--json', env=env)['running']:
                break
            time.sleep(0.02)
        else:
            self.fail('registered launch did not use selected paths')
        self.assertFalse(self.state.exists())
        self.assertFalse(self.cli('desktop', 'status', '--json')['running'])
        self.cli('desktop', 'serve', '--config-dir', 'relative', '--state-dir', str(state), ok=False)
        self.cli('@example', 'desktop', 'serve', '--config-dir', str(selected), '--state-dir', str(state), ok=False)
        self.assertFalse(self.state.exists(), 'a prefixed invocation must never ignore registered paths')

    def test_passive_kernel_exclusion_and_restart(self):
        process, first = self.serve()
        self.assertEqual(first['ownership'], 'passive')
        self.assertFalse(first['writers_enabled'])
        self.assertFalse(first['automatic_approvals'])
        self.assertEqual(first['upstreams'], [])
        self.assertFalse(first['complete'])
        self.failed_serve()
        self.assertEqual(self.cli('desktop', 'status', '--json')['epoch'], first['epoch'])
        self.stop(process)
        _, second = self.serve()
        self.assertNotEqual(first['epoch'], second['epoch'])
        self.assertEqual(second['generation'], first['generation'] + 1)
        self.assertFalse((self.state / 'desktop-presence.json').exists())
        self.assertFalse((self.state / 'read-marks.json').exists())
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_background_fleet_lifetime_revoke_and_stale_epoch(self):
        self.env['HGS_PEERS'] = ''
        # Exercise unavailable inputs without consulting the owner's desktop.
        self.env['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(self.root / 'missing-bus')
        self.env['XDG_RUNTIME_DIR'] = str(self.root)
        self.env['XDG_SESSION_TYPE'] = 'wayland'
        self.env['WAYLAND_DISPLAY'] = 'missing-wayland'
        process, first = self.serve(background=True)
        self.assertEqual(first['ownership'], 'background_service')
        self.assertTrue(first['writers_enabled'])
        watcher, hello, frames = self.watch()
        self.assertEqual(hello['value']['ownership'], 'background_service')
        deadline = time.monotonic() + 10
        current = first
        while time.monotonic() < deadline:
            current = self.cli('desktop', 'status', '--json')
            if current['background_runtime']['transport_ready']:
                break
            time.sleep(0.02)
        self.assertTrue(current['background_runtime']['transport_ready'], current)
        self.assertTrue(current['lifecycle']['background']['fleet']['ready'])
        self.assertEqual(current['background_runtime']['workers'], 1)
        self.assertEqual(current['attention_counts']['aggregate']['attention'], 0)
        self.assertTrue(current['attention_counts']['aggregate']['complete'])
        self.assertEqual(current['maintenance']['abi'], 'desktop-maintenance-v1')
        self.assertIsNone(current['maintenance']['configuration'])
        self.assertFalse(current['maintenance']['updates']['busy'])
        self.assertFalse(current['attention_discovery']['enabled'])
        self.assertFalse(current['attention_discovery']['optional_questions'])
        self.assertFalse(current['attention_discovery']['stopping'])
        self.assertEqual(current['attention_discovery']['sources'], 0)
        self.assertEqual(current['attention_discovery']['pending_rows'], 0)
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_attention_edges').fetchone()[0], 0)
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_intents').fetchone()[0], 0)
        text = subprocess.run([str(HGS), 'desktop', 'status'], env=self.env,
                              capture_output=True, text=True, timeout=10)
        self.assertEqual(text.returncode, 0, text.stderr)
        self.assertIn('background fleet', text.stdout)
        for enabled in (True, False):
            result = self.cli('desktop', 'request', '--json', data={
                'op': 'automatic_approvals', 'request_id': str(uuid.uuid4()), 'enabled': enabled})
            self.assertFalse(result['accepted'], 'generic CLI must not change approval policy')
        legacy = self.cli('presence', 'set', '--json', data={'idle_seconds': 0, 'locked': False}, ok=False)
        self.assertNotEqual(legacy.returncode, 0, 'service must exclude the legacy writer')
        request_id = str(uuid.uuid4())
        result = self.cli('desktop', 'revoke', '--json', '--request-id', request_id)
        self.assertTrue(result['revoked'])
        self.assertEqual(result['writer_epoch'], first['epoch'])
        stopped = self.cli('desktop', 'status', '--json')
        self.assertTrue(stopped['background_runtime']['drained'])
        self.assertEqual(stopped['ownership'], 'revoked')
        self.assertFalse(stopped['maintenance']['ready'])
        self.assertTrue(stopped['attention_discovery']['stopping'])
        self.assertEqual(stopped['attention_discovery']['pending_rows'], 0)
        self.assertTrue(stopped['lifecycle']['background']['presence']['drained'])
        self.assertFalse(stopped['writers_enabled'])
        self.cli('presence', 'set', '--json', data={'idle_seconds': 0, 'locked': False})
        watcher.stdin.close()
        watcher.wait(timeout=5)
        self.stop(process)
        replacement, second = self.serve(background=True)
        self.assertNotEqual(first['epoch'], second['epoch'])
        self.assertEqual(second['ownership'], 'passive')
        self.assertFalse(second['writers_enabled'])
        self.assertEqual(second['background_start']['code'], 'explicit_handoff_required')
        self.assertNotIn('background_runtime', second)
        self.assertTrue(all(not component['ready'] for component in second['lifecycle']['background'].values()))
        stale = self.cli('desktop', 'revoke', '--json', '--request-id', request_id, ok=False)
        self.assertEqual(json.loads(stale.stdout)['code'], 'writer_epoch_changed')
        self.assertEqual(self.cli('desktop', 'status', '--json')['ownership'], 'passive')

    def test_frontend_policy_requires_a_registered_original_ui_child(self):
        self.serve()
        help_text = subprocess.run([str(HGS), '--help'], env=self.env,
                                   capture_output=True, text=True, timeout=10)
        self.assertIn('Desktop frontend request ABI: desktop-ui-request-v1', help_text.stdout)
        for enabled in (True, False):
            request = {'op': 'automatic_approvals', 'request_id': str(uuid.uuid4()), 'enabled': enabled}
            generic = self.cli('desktop', 'request', '--json', data=request)
            self.assertFalse(generic['accepted'])
            envelope = {'abi': 'desktop-ui-request-v1', 'attempt': str(uuid.uuid4()), 'request': request}
            refused = self.cli('desktop', 'frontend-request', '--json', data=envelope)
            self.assertFalse(refused['accepted'])
            self.assertFalse(refused['durable'])
            self.assertEqual(refused['code'], 'frontend_authentication_refused')
            for field in ('confirmed', 'role', 'caller_pid'):
                claimed = dict(envelope, **{field: True})
                self.assertFalse(self.cli('desktop', 'frontend-request', '--json', data=claimed)['accepted'])
            tagged = dict(self.env, HGS_SESSION='codex/example', HGS_RUN_ID=str(uuid.uuid4()))
            attempted = subprocess.run([str(HGS), 'desktop', 'frontend-request', '--json'],
                                       env=tagged, input=json.dumps(envelope), capture_output=True,
                                       text=True, timeout=10)
            if attempted.returncode == 0:
                self.assertFalse(json.loads(attempted.stdout)['accepted'])
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT enabled,revision,requests FROM desktop_delivery_policy').fetchone(), (0, 0, 0))
        receipt = self.cli('desktop', 'request', '--json', data={
            'op': 'delivery_result', 'request_id': str(uuid.uuid4()),
            'original_request_id': str(uuid.uuid4())})
        self.assertTrue(receipt['accepted'])
        self.assertFalse(receipt['found'])
        self.assertFalse(receipt['durable'])

    def test_maintenance_configure_requires_native_authority_and_result_is_readonly(self):
        self.serve()
        help_text = subprocess.run([str(HGS), '--help'], env=self.env,
                                   capture_output=True, text=True, timeout=10)
        self.assertIn('Desktop maintenance ABI: desktop-maintenance-v1', help_text.stdout)
        request = {'op': 'maintenance_configure', 'request_id': str(uuid.uuid4()),
                   'update': None, 'recovery': None}
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            before = db.execute('SELECT value FROM desktop_maintenance WHERE id=1').fetchone()[0]
        self.assertFalse(self.cli('desktop', 'request', '--json', data=request)['accepted'])
        envelope = {'abi': 'desktop-ui-request-v1', 'attempt': str(uuid.uuid4()), 'request': request}
        self.assertFalse(self.cli('desktop', 'frontend-request', '--json', data=envelope)['accepted'])
        result = self.cli('desktop', 'request', '--json', data={
            'op': 'maintenance_result', 'request_id': str(uuid.uuid4()),
            'original_request_id': request['request_id']})
        self.assertFalse(result['accepted'])
        self.assertEqual(result['code'], 'unknown_request')
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT value FROM desktop_maintenance WHERE id=1').fetchone()[0], before)

    def test_lifecycle_probe_binds_the_live_service_and_nondefault_configuration(self):
        missing = self.cli('desktop', 'status', '--json')['lifecycle']
        self.assertFalse(missing['ipc_ready'])
        self.assertEqual(missing['phase'], 'unavailable')
        self.assertIsNone(missing['epoch'])
        process, first = self.serve()
        lifecycle = first['lifecycle']
        self.assertTrue(lifecycle['ipc_ready'])
        self.assertEqual(lifecycle['abi'], 'desktop-lifecycle-v1')
        self.assertEqual(lifecycle['pid'], process.pid)
        for key in ('configuration', 'epoch', 'generation'):
            self.assertEqual(lifecycle[key], first[key])
        self.assertTrue(all(not component['ready'] for component in lifecycle['background'].values()))
        registration = self.cli('desktop', 'registration', '--json')
        probe = registration['readiness']
        self.assertEqual(probe['expected_configuration'], first['configuration'])
        self.assertEqual(probe['environment'], dict(HGS_CONFIG_DIR=str(self.config), HGS_STATE_DIR=str(self.state)))
        result = subprocess.run(probe['argv'], env={**self.env, **probe['environment']},
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['lifecycle'], lifecycle)
        self.assertFalse((self.root / 'unexpected-tool').exists())

    @unittest.skipUnless(sys.platform.startswith('linux'), 'Linux installer consumer')
    def test_linux_installer_consumes_the_real_scoped_readiness_probe(self):
        import runpy
        script = Path(__file__).resolve().parents[1] / 'scripts/install-linux-desktop-service.py'
        consumer = runpy.run_path(str(script))['Installer']
        process, status = self.serve()
        selected = consumer(str(HGS), self.env)
        live = selected.probe()
        self.assertTrue(live['ipc_ready'])
        self.assertEqual(live['pid'], process.pid)
        self.assertEqual(live['configuration'], status['configuration'])
        self.assertFalse(any(value['ready'] for value in live['background'].values()))
        other = dict(self.env, HGS_CONFIG_DIR=str(self.root / 'other-config'),
                     HGS_STATE_DIR=str(self.root / 'other-state'))
        absent = consumer(str(HGS), other).probe()
        self.assertFalse(absent['ipc_ready'])
        self.assertNotEqual(absent['configuration'], live['configuration'])
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_activation_payload_is_unavailable_to_generic_and_unregistered_children(self):
        self.serve()
        request_id = str(uuid.uuid4())
        request = dict(op='activation_wait', request_id=request_id)
        generic = self.cli('desktop', 'request', '--json', data=request)
        self.assertFalse(generic['accepted'])
        envelope = dict(abi='desktop-ui-request-v1', attempt=str(uuid.uuid4()), request=request)
        child = self.cli('desktop', 'frontend-request', '--json', data=envelope)
        self.assertEqual(child['request_id'], request_id)
        self.assertFalse(child['accepted'])
        self.assertEqual(child['code'], 'frontend_authentication_refused')
        self.assertNotIn('activation', child)
        help_result = subprocess.run([str(HGS), '--help'], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertIn('Desktop activation ABI: desktop-activation-v1', help_result.stdout)

    def test_queries_and_effect_refusal_have_exact_request_ids(self):
        self.serve()
        request_id = str(uuid.uuid4())
        result = self.cli('desktop', 'request', '--json', data={'op': 'ping', 'request_id': request_id})
        self.assertEqual(result, dict(request_id=request_id, accepted=True, durable=False, op='pong'))
        for op in ('read', 'approval_settings', 'focus', 'activate', 'subscribe', 'maintenance_configure', 'unknown'):
            result = self.cli('desktop', 'request', '--json', data={'op': op, 'request_id': request_id, 'enabled': True})
            self.assertEqual(result, dict(request_id=request_id, accepted=False, code='handoff_required'))
        result = self.cli('desktop', 'request', '--json', data={
            'op': 'maintenance_result', 'request_id': request_id, 'enabled': True})
        self.assertEqual(result, dict(request_id=request_id, accepted=False,
                                     durable=False, code='maintenance_refused'))
        result = self.cli('desktop', 'request', '--json', data={'op': 'ping', 'request_id': 'not-a-uuid'})
        self.assertEqual(result['code'], 'invalid_request_id')
        self.assertFalse((self.desktop / 'approval-log').exists())

    def import_evidence(self, phase='complete', evidence='drained'):
        configuration = self.cli('desktop', 'status', '--json')['configuration']
        import_id = '11111111-1111-4111-8111-111111111111'
        folder = self.desktop / 'imports' / import_id
        folder.mkdir(parents=True, exist_ok=True, mode=0o700)
        for directory in (self.state, self.desktop, folder.parent, folder):
            directory.chmod(0o700)
        record = json.dumps(dict(version=1, configuration=configuration, source='com.hgdev.hgs-tray',
                                 import_id=import_id, evidence=evidence, created_at=1,
                                 automatic_approvals=False, sources=[], pages=[])).encode()
        record_path = folder / 'record-v1.json'
        record_path.write_bytes(record)
        record_path.chmod(0o600)
        marker = dict(version=1, configuration=configuration, source='com.hgdev.hgs-tray', import_id=import_id,
                      writer_epoch='22222222-2222-4222-8222-222222222222', phase=phase,
                      import_record=dict(version=1, sha256=hashlib.sha256(record).hexdigest()), automatic_approvals=False)
        marker_path = self.desktop / 'handoff-v1.json'
        marker_path.write_text(json.dumps(marker))
        marker_path.chmod(0o600)
        return marker_path, record_path, marker

    def assert_passive_import(self, reason):
        process, status = self.serve()
        self.assertEqual(status['migration']['reason'], reason)
        self.assertFalse(status['writers_enabled'])
        self.assertFalse(status['automatic_approvals'])
        self.assertEqual(status['upstreams'], [])
        result = self.cli('desktop', 'request', '--json', data={'op': 'read', 'request_id': str(uuid.uuid4())})
        self.assertFalse(result['accepted'])
        self.stop(process)
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_import_phases_never_substitute_for_current_writer_proof(self):
        for phase, evidence, reason in [('revoked', 'drained', 'revoked'),
                                        ('imported', 'drained', 'handoff_incomplete'),
                                        ('quiesced', 'drained', 'handoff_incomplete'),
                                        ('complete', 'staged', 'import_not_drained'),
                                        ('complete', 'drained', 'delegation_required')]:
            with self.subTest(phase=phase, evidence=evidence):
                marker, record, _ = self.import_evidence(phase, evidence)
                before = marker.read_bytes(), record.read_bytes()
                self.assert_passive_import(reason)
                self.assertEqual((marker.read_bytes(), record.read_bytes()), before)
        # Even a second start with a complete, drained record stays passive.
        self.assert_passive_import('delegation_required')

    def test_invalid_or_newer_handoff_is_preserved_and_refused(self):
        changes = [dict(version=2), dict(phase='future'), dict(configuration='b' * 64),
                   dict(source='dev.zerus.next'), dict(automatic_approvals=True),
                   dict(import_id='../escape'), dict(unknown='field')]
        for change in changes:
            with self.subTest(change=change):
                path, record, marker = self.import_evidence()
                marker.update(change)
                path.write_text(json.dumps(marker))
                before = path.read_bytes(), record.read_bytes()
                self.assert_passive_import('invalid_import_evidence')
                self.assertEqual((path.read_bytes(), record.read_bytes()), before)
        for contents in (b'{', b' ' * (64 * 1024 + 1)):
            path, _, _ = self.import_evidence()
            path.write_bytes(contents)
            self.assert_passive_import('invalid_import_evidence')
            self.assertEqual(path.read_bytes(), contents)

    def test_import_manifest_digest_size_and_private_leaf_are_checked(self):
        for contents in (b'{}', b' ' * (256 * 1024 + 1)):
            _, record, _ = self.import_evidence()
            record.write_bytes(contents)
            self.assert_passive_import('invalid_import_evidence')
            self.assertEqual(record.read_bytes(), contents)
        _, record, _ = self.import_evidence()
        record.unlink()
        self.assert_passive_import('invalid_import_evidence')
        if os.name != 'nt':
            marker, record, _ = self.import_evidence()
            record.chmod(0o644)
            self.assert_passive_import('invalid_import_evidence')
            record.chmod(0o600)
            saved = marker.read_bytes()
            marker.unlink()
            marker.symlink_to(record)
            self.assert_passive_import('invalid_import_evidence')
            self.assertTrue(marker.is_symlink())
            marker.unlink()
            os.mkfifo(marker, 0o600)
            self.assert_passive_import('invalid_import_evidence')
            marker.unlink()
            marker.write_bytes(saved)
            marker.chmod(0o600)
            sibling = record.parent.with_name('foreign-import')
            record.parent.rename(sibling)
            record.parent.symlink_to(sibling, target_is_directory=True)
            self.assert_passive_import('invalid_import_evidence')
            self.assertTrue(record.parent.is_symlink())

    @unittest.skipIf(os.name == 'nt', 'POSIX flock import-commit contention fixture')
    def test_import_commit_lock_is_separate_and_contention_stays_passive(self):
        import fcntl
        self.import_evidence()
        lock_path = self.desktop / 'handoff.lock'
        with lock_path.open('w') as lock:
            lock_path.chmod(0o600)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assert_passive_import('handoff_unavailable')
        self.assert_passive_import('delegation_required')

    def test_two_clients_share_epoch_and_eof_detaches_only_one(self):
        service, status = self.serve()
        first, a, _ = self.watch()
        second, b, _ = self.watch()
        self.assertEqual(a['epoch'], status['epoch'])
        self.assertEqual(b['epoch'], a['epoch'])
        self.assertEqual((a['t'], b['t']), ('hello', 'hello'))
        first.stdin.close()
        first.wait(timeout=5)
        self.assertEqual(first.returncode, 0, first.stderr.read())
        self.assertIsNone(second.poll())
        self.assertIsNone(service.poll())
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if self.cli('desktop', 'status', '--json')['subscribers'] == 1:
                break
            time.sleep(0.02)
        else:
            self.fail('closed client retained its subscriber slot')

    def test_subscriber_limit_preserves_status_lane(self):
        self.serve()
        for _ in range(8):
            _, hello, _ = self.watch()
            self.assertEqual(hello['t'], 'hello')
        ninth, refusal, _ = self.watch()
        self.assertEqual(refusal['value']['code'], 'subscriber_limit')
        ninth.wait(timeout=5)
        self.assertEqual(self.cli('desktop', 'status', '--json')['subscribers'], 8)

    def test_watch_stays_attached_across_two_heartbeats(self):
        service, status = self.serve()
        watcher, hello, frames = self.watch()
        for sequence in (hello['seq'] + 1, hello['seq'] + 2):
            heartbeat = frames.get(timeout=13)
            self.assertEqual(heartbeat['t'], 'heartbeat')
            self.assertEqual(heartbeat['epoch'], status['epoch'])
            self.assertEqual(heartbeat['seq'], sequence)
            self.assertIsNone(watcher.poll(), watcher.stderr.read() if watcher.poll() is not None else '')
            self.assertEqual(self.cli('desktop', 'status', '--json')['subscribers'], 1)
        self.stop(service)
        watcher.wait(timeout=5)
        self.assertEqual(watcher.returncode, 0)

    def test_newer_schema_is_preserved(self):
        self.desktop.mkdir(mode=0o700, parents=True)
        self.state.chmod(0o700)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            db.execute('PRAGMA user_version=999')
            db.execute('CREATE TABLE future (payload TEXT)')
            db.execute("INSERT INTO future VALUES ('preserved')")
        database.chmod(0o600)
        before = database.read_bytes()
        self.assertIn('newer hgs', self.failed_serve().stderr)
        self.assertEqual(database.read_bytes(), before)
        self.assertFalse((self.desktop / 'state.sqlite3-wal').exists())

    def test_another_configuration_cannot_reuse_the_store(self):
        process, first = self.serve()
        other = self.root / 'other-config'
        other.mkdir(mode=0o700)
        env = dict(self.env, HGS_CONFIG_DIR=str(other))
        self.cli('desktop', 'status', '--json', env=env, ok=False)
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        before = database.read_bytes()
        self.assertIn('another configuration', self.failed_serve(env=env).stderr)
        self.assertEqual(before, database.read_bytes())
        _, second = self.serve()
        self.assertEqual(second['generation'], first['generation'] + 1)

    def test_schema_one_migration_preserves_configuration_and_generation(self):
        configuration = self.cli('desktop', 'status', '--json')['configuration']
        self.desktop.mkdir(mode=0o700, parents=True)
        self.state.chmod(0o700)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            db.execute('CREATE TABLE service (id INTEGER PRIMARY KEY CHECK(id=1), configuration TEXT NOT NULL, generation INTEGER NOT NULL, epoch TEXT NOT NULL)')
            db.execute('INSERT INTO service VALUES(1,?,7,?)', (configuration, str(uuid.uuid4())))
            db.execute('PRAGMA user_version=1')
        database.chmod(0o600)
        process, status = self.serve()
        self.assertEqual(status['schema'], SCHEMA)
        self.assertEqual(status['generation'], 8)
        self.assertEqual(status['configuration'], configuration)
        self.assertTrue(status['effect_journal']['revoked'])
        self.assertEqual(status['effect_journal']['attempts']['reserved'], 0)
        self.assertEqual(status['read_queue']['pending'], 0)
        self.assertTrue(status['read_queue']['admissions_paused'])
        self.assertFalse(status['read_queue']['recovery_error'])
        self.stop(process)
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], SCHEMA)
            self.assertEqual(db.execute('SELECT rows,bytes FROM desktop_catalog_totals').fetchone(), (0, 0))
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_catalog_sources').fetchone()[0], 0)
            self.assertEqual(db.execute('SELECT configuration FROM service').fetchone()[0], configuration)

    @staticmethod
    def remove_cache_schema(db):
        for trigger in ('desktop_cache_unknown_preserve', 'desktop_cache_unknown_immutable'):
            db.execute(f'DROP TRIGGER {trigger}')
        for table in ('desktop_cache_unknown', 'desktop_cache_files', 'desktop_cache_operations', 'desktop_cache_roots'):
            db.execute(f'DROP TABLE {table}')

    @classmethod
    def remove_changes_schema(cls, db):
        cls.remove_cache_schema(db)
        db.execute('DROP TABLE desktop_creation_clones')
        db.execute('DROP TABLE desktop_creation_clone_budget')
        db.execute('DROP TABLE desktop_notification_preferences_requests')
        db.execute('DROP TABLE desktop_notification_preferences')
        db.execute('DROP TABLE desktop_ownership_requests')
        db.execute('DROP TABLE desktop_ownership')
        db.execute('DROP INDEX desktop_attention_catalog_identity')
        for table in ('desktop_attention_edges', 'desktop_attention_sessions', 'desktop_attention_totals', 'desktop_catalog_changes', 'desktop_catalog_change_views', 'desktop_catalog_change_totals'):
            db.execute(f'DROP TABLE {table}')

    @classmethod
    def remove_counts_and_delivery_schema(cls, db):
        # Rewind every later layer, rather than leaving a current database
        # labelled as an older version. Production migrations stay strict.
        cls.remove_changes_schema(db)
        db.execute('DROP TABLE desktop_maintenance')
        db.execute('DROP INDEX desktop_delivery_catalog_name')
        db.execute('DROP TABLE desktop_delivery_requests')
        db.execute('DROP TABLE desktop_delivery_policy')
        for suffix in ('generation', 'insert', 'delete', 'update'):
            db.execute(f'DROP TRIGGER desktop_counts_{suffix}')
        db.execute('ALTER TABLE desktop_catalog_rows DROP COLUMN shared_flags')
        for column in ('counts_ready', 'visible_sessions', 'attention_sessions'):
            db.execute(f'ALTER TABLE desktop_catalog_generations DROP COLUMN {column}')

    def test_cache_leaf_migration_is_atomic_and_keeps_the_service_passive(self):
        process, before = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        previous = 17  # Historical predecessor of the original cache-18 leaf.
        with closing(sqlite3.connect(database)) as db, db:
            self.remove_cache_schema(db)
            db.execute(f'PRAGMA user_version={previous}')
            db.execute('CREATE TABLE desktop_cache_roots(fixture TEXT)')
            db.execute("INSERT INTO desktop_cache_roots VALUES('retained')")
        self.failed_serve()
        with closing(sqlite3.connect(database)) as db, db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], previous)
            self.assertEqual(db.execute('SELECT fixture FROM desktop_cache_roots').fetchone()[0], 'retained')
            self.assertEqual(db.execute("SELECT count(*) FROM sqlite_schema WHERE name='desktop_cache_operations'").fetchone()[0], 0)
            db.execute('DROP TABLE desktop_cache_roots')
        process, status = self.serve()
        self.assertEqual(status['schema'], SCHEMA)
        self.assertEqual(status['configuration'], before['configuration'])
        self.assertFalse(status['writers_enabled'])
        self.assertFalse(status['automatic_approvals'])
        self.assertEqual(status['operation_queue']['undrained'], 0)
        self.stop(process)
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_cache_operations').fetchone()[0], 0)
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_cache_unknown').fetchone()[0], 0)

    def test_cache_effect_migration_is_atomic_and_keeps_existing_cache_tables(self):
        process, before = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            db.execute('DROP TRIGGER desktop_cache_effect_monotone')
            db.execute('DROP TRIGGER desktop_cache_kept_original')
            db.execute('ALTER TABLE desktop_cache_operations DROP COLUMN effect_started')
            db.execute('ALTER TABLE desktop_cache_operations DROP COLUMN kept_receipt')
            db.execute('PRAGMA user_version=18')
            db.execute('CREATE TRIGGER desktop_cache_effect_monotone BEFORE UPDATE ON desktop_cache_operations BEGIN SELECT 1; END')
        self.failed_serve()
        with closing(sqlite3.connect(database)) as db, db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 18)
            self.assertEqual(db.execute("SELECT count(*) FROM pragma_table_info('desktop_cache_operations') WHERE name IN('effect_started','kept_receipt')").fetchone()[0], 0)
            db.execute('DROP TRIGGER desktop_cache_effect_monotone')
        process, status = self.serve()
        self.assertEqual(status['schema'], SCHEMA)
        self.assertEqual(status['configuration'], before['configuration'])
        self.assertFalse(status['writers_enabled'])
        self.assertFalse(status['automatic_approvals'])
        self.assertEqual(status['operation_queue']['undrained'], 0)

    def test_schema_five_metadata_is_charged_once_and_preserved(self):
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        hello = json.dumps({'machine_id': 'example-machine'})
        meta = json.dumps({'label': 'example-\u03bb'}, ensure_ascii=False)
        expected = len(hello.encode()) + len(meta.encode())
        with closing(sqlite3.connect(database)) as db, db:
            for suffix in ('insert', 'update', 'delete'):
                db.execute(f'DROP TRIGGER desktop_catalog_source_{suffix}')
            db.execute('INSERT INTO desktop_catalog_sources(computer,epoch,incarnation,generation,stream,seq,hello,meta) VALUES(?,?,?,?,?,?,?,?)',
                       ('peer', str(uuid.uuid4()), 1, 1, 'old-watch', 1, hello, meta))
            db.execute('DROP INDEX desktop_commands_pending')
            for table in ('desktop_producer_prepared', 'desktop_operation_custody', 'desktop_operation_attempts', 'desktop_operations', 'desktop_operation_totals'):
                db.execute(f'DROP TABLE {table}')
            self.remove_counts_and_delivery_schema(db)
            db.execute('PRAGMA user_version=5')
        for _ in range(2):
            process, status = self.serve()
            self.assertEqual(status['schema'], SCHEMA)
            self.assertEqual(status['catalog_projection']['stored_bytes'], expected)
            self.assertEqual(status['catalog_projection']['stored_rows'], 0)
            self.stop(process)
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute('SELECT hello,meta FROM desktop_catalog_sources').fetchone(), (hello, meta))
            self.assertEqual(db.execute('SELECT rows,bytes FROM desktop_catalog_totals').fetchone(), (0, expected))
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_catalog_sources').fetchone()[0], 1)

    def test_schema_six_operation_migration_is_atomic_and_passive(self):
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            db.execute('DROP INDEX desktop_commands_pending')
            for table in ('desktop_producer_prepared', 'desktop_operation_custody', 'desktop_operation_attempts', 'desktop_operations', 'desktop_operation_totals'):
                db.execute(f'DROP TABLE {table}')
            self.remove_counts_and_delivery_schema(db)
            db.execute('PRAGMA user_version=6')
            db.execute('CREATE TABLE desktop_operation_totals (fixture TEXT)')
        self.failed_serve()
        with closing(sqlite3.connect(database)) as db, db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 6)
            self.assertEqual(db.execute("SELECT count(*) FROM sqlite_schema WHERE name='desktop_operations'").fetchone()[0], 0)
            db.execute('DROP TABLE desktop_operation_totals')
        process, status = self.serve()
        self.assertEqual(status['schema'], SCHEMA)
        self.assertEqual({key:status['operation_queue'][key] for key in ('unsettled','undrained','admissions_paused','recovery_error')}, dict(unsettled=0, undrained=0, admissions_paused=True, recovery_error=False))
        self.assertFalse(status['writers_enabled'])
        self.stop(process)

    def test_schema_seven_custody_migration_is_atomic_and_creates_no_lock_files(self):
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            db.execute('DROP TABLE desktop_producer_prepared')
            db.execute('DROP TABLE desktop_operation_custody')
            db.execute('DROP INDEX desktop_commands_pending')
            self.remove_counts_and_delivery_schema(db)
            db.execute('PRAGMA user_version=7')
            db.execute('CREATE INDEX desktop_commands_pending ON desktop_commands(request_id)')
        self.failed_serve()
        with closing(sqlite3.connect(database)) as db, db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 7)
            self.assertEqual(db.execute("SELECT count(*) FROM sqlite_schema WHERE name='desktop_operation_custody'").fetchone()[0], 0)
            self.assertEqual(db.execute("SELECT count(*) FROM sqlite_schema WHERE name LIKE 'desktop_custody_%'").fetchone()[0], 0)
            db.execute('DROP INDEX desktop_commands_pending')
        process, status = self.serve()
        self.assertEqual(status['schema'], SCHEMA)
        self.assertEqual(status['operation_queue']['undrained'], 0)
        self.assertTrue(status['operation_queue']['admissions_paused'])
        self.assertFalse((self.desktop / 'operation-custody').exists())
        self.stop(process)

    def test_schema_eight_preparation_migration_is_atomic_and_passive(self):
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            db.execute('DROP TABLE desktop_producer_prepared')
            self.remove_counts_and_delivery_schema(db)
            db.execute('PRAGMA user_version=8')
            db.execute('CREATE TABLE desktop_producer_prepared (fixture TEXT)')
            db.execute("INSERT INTO desktop_producer_prepared VALUES('retained')")
        self.failed_serve()
        with closing(sqlite3.connect(database)) as db, db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 8)
            self.assertEqual(db.execute('SELECT fixture FROM desktop_producer_prepared').fetchone()[0], 'retained')
            db.execute('DROP TABLE desktop_producer_prepared')
        process, status = self.serve()
        self.assertEqual(status['schema'], SCHEMA)
        self.assertFalse(status['writers_enabled'])
        self.stop(process)
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], SCHEMA)
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_producer_prepared').fetchone()[0], 0)

    def test_schema_eleven_maintenance_migration_stays_passive(self):
        process, old = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            self.remove_changes_schema(db)
            db.execute('DROP TABLE desktop_maintenance')
            db.execute('PRAGMA user_version=11')
        process, status = self.serve()
        self.assertEqual(status['schema'], SCHEMA)
        self.assertEqual(status['generation'], old['generation'] + 1)
        self.assertEqual(status['configuration'], old['configuration'])
        self.assertFalse(status['writers_enabled'])
        self.stop(process)
        with closing(sqlite3.connect(database)) as db:
            stored = json.loads(db.execute('SELECT value FROM desktop_maintenance WHERE id=1').fetchone()[0])
            self.assertIsNone(stored['configuration'])
            self.assertEqual(stored['receipts'], {})
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_schema_eleven_maintenance_migration_refuses_a_conflicting_table(self):
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            self.remove_changes_schema(db)
            db.execute('DROP TABLE desktop_maintenance')
            db.execute('CREATE TABLE desktop_maintenance(fixture TEXT)')
            db.execute("INSERT INTO desktop_maintenance VALUES('retained')")
            db.execute('PRAGMA user_version=11')
        self.failed_serve()
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 11)
            self.assertEqual(db.execute('SELECT fixture FROM desktop_maintenance').fetchone()[0], 'retained')
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_schema_twelve_change_journal_refuses_conflicts_atomically(self):
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            self.remove_changes_schema(db)
            db.execute('CREATE TABLE desktop_catalog_change_views(fixture TEXT)')
            db.execute("INSERT INTO desktop_catalog_change_views VALUES('retained')")
            db.execute('PRAGMA user_version=12')
        self.failed_serve()
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 12)
            self.assertEqual(db.execute('SELECT fixture FROM desktop_catalog_change_views').fetchone()[0], 'retained')
            self.assertEqual(db.execute("SELECT count(*) FROM sqlite_schema WHERE name='desktop_catalog_changes'").fetchone()[0], 0)
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_maintenance').fetchone()[0], 1)

    def test_catalog_collection_progresses_without_a_frontend(self):
        process, _ = self.serve()
        self.stop(process)
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db, db:
            db.execute("INSERT INTO desktop_catalog_generations(computer,obsolete) VALUES('peer',1)")
            generation = db.execute('SELECT last_insert_rowid()').fetchone()[0]
            db.create_function('desktop_shared_flags', 1, lambda raw: 0)
            db.executemany('INSERT INTO desktop_catalog_rows(generation,key,json,bytes) VALUES(?,?,?,?)',
                           ((generation, f'run:{n}', '{}', 64) for n in range(768)))
        _, status = self.serve()
        deadline = time.monotonic() + 5
        while status['catalog_projection']['stored_rows'] and time.monotonic() < deadline:
            time.sleep(0.02)
            status = self.cli('desktop', 'status', '--json')
            self.assertEqual(status['subscribers'], 0)
            self.assertFalse(status['writers_enabled'])
        self.assertEqual(status['catalog_projection']['stored_rows'], 0)
        self.assertEqual(status['catalog_projection']['stored_bytes'], 0)
        self.assertEqual(status['catalog_projection']['obsolete_generations'], 0)
        self.assertFalse(status['catalog_projection']['collection_error'])
        self.assertTrue(status['effect_journal']['revoked'])
        self.assertEqual(status['read_queue']['pending'], 0)

    def test_revoke_without_handoff_or_service_writes_nothing(self):
        help_result = subprocess.run([str(HGS), '--help'], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(help_result.returncode, 0, help_result.stderr)
        self.assertIn('Desktop revoke ABI: desktop-revoke-v1', help_result.stdout)
        self.assertFalse(self.state.exists())
        request_id = str(uuid.uuid4())
        result = self.cli('desktop', 'revoke', '--json', '--request-id', request_id, '--reason', 'rollback')
        self.assertEqual(result['handoff'], 'none')
        self.assertFalse(result['applicable'])
        self.assertTrue(result['nothing_written'])
        self.assertTrue(result['revoked'])
        self.assertEqual(result['request_id'], request_id)
        self.assertFalse(self.state.exists())
        self.assertFalse((self.root / 'unexpected-tool').exists())
        # Pre-existing native state or an unused desktop folder stays unchanged.
        self.state.mkdir(mode=0o700)
        preserved = self.state / 'foreign-state'
        preserved.write_text('preserved')
        for desktop in (False, True):
            if desktop:
                self.desktop.mkdir(mode=0o700)
            self.assertEqual(self.cli('desktop', 'revoke', '--json', '--request-id', request_id, '--reason', 'rollback'), result)
            self.assertEqual(preserved.read_text(), 'preserved')
            if desktop:
                self.assertEqual(list(self.desktop.iterdir()), [])
        orphan = self.desktop / 'state.sqlite3-wal'
        orphan.write_bytes(b'preserved orphan')
        self.cli('desktop', 'revoke', '--json', ok=False)
        self.assertEqual(orphan.read_bytes(), b'preserved orphan')
        self.assertFalse((self.desktop / 'owner.lock').exists())

    def test_absent_service_with_existing_state_revokes_without_starting_anything(self):
        process, _ = self.serve()
        self.stop(process)
        request_id = str(uuid.uuid4())
        result = self.cli('desktop', 'revoke', '--json', '--request-id', request_id, '--reason', 'rollback')
        self.assertTrue(result['revoked'])
        self.assertTrue(result['checkpointed'])
        self.assertTrue(result['durable'])
        self.assertEqual(result['request_id'], request_id)
        self.assertIsNone(result['writer_epoch'])
        self.assertFalse((self.desktop / 'handoff-v1.json').exists())
        self.assertFalse((self.desktop / 'service.sock').exists())
        self.assertFalse(self.cli('desktop', 'status', '--json')['running'])
        self.assertEqual(self.cli('desktop', 'revoke', '--json', '--request-id', request_id, '--reason', 'rollback'), result)
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_live_revoke_preserves_import_identity_and_has_an_exact_receipt(self):
        path, record, marker = self.import_evidence()
        before_record = record.read_bytes()
        service, status = self.serve()
        request_id = str(uuid.uuid4())
        result = self.cli('desktop', 'revoke', '--json', '--request-id', request_id, '--reason', 'rollback')
        self.assertTrue(result['revoked'])
        self.assertTrue(result['checkpointed'])
        self.assertEqual(result['writer_epoch'], marker['writer_epoch'])
        marker['phase'] = 'revoked'
        self.assertEqual(json.loads(path.read_text(encoding='utf-8')), marker)
        self.assertEqual(record.read_bytes(), before_record)
        query = dict(op='revoke_status', request_id=request_id)
        self.assertEqual(self.cli('desktop', 'request', '--json', data=query), result)
        current = self.cli('desktop', 'status', '--json')
        self.assertEqual(current['epoch'], status['epoch'])
        self.assertEqual(current['migration']['reason'], 'revoked')
        self.assertEqual(current['effect_journal']['reason'], 'manual_rollback')
        self.assertFalse(current['writers_enabled'])
        self.stop(service)
        self.serve()
        self.assertEqual(self.cli('desktop', 'request', '--json', data=query), result)
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_revoke_replay_conflict_and_later_epoch_refuse_success(self):
        path, _, marker = self.import_evidence()
        self.serve()
        request_id = str(uuid.uuid4())
        args = ['desktop', 'revoke', '--json', '--request-id', request_id]
        self.assertTrue(self.cli(*args)['revoked'])
        refused = self.cli(*args, '--reason', 'rollback', ok=False)
        self.assertEqual(json.loads(refused.stdout)['code'], 'request_conflict')
        marker['writer_epoch'] = str(uuid.uuid4())
        path.write_text(json.dumps(marker))
        before = path.read_bytes()
        refused = self.cli(*args, ok=False)
        self.assertEqual(json.loads(refused.stdout)['code'], 'writer_epoch_changed')
        self.assertEqual(path.read_bytes(), before)
        self.assertTrue(self.cli('desktop', 'revoke', '--json')['revoked'])

    @unittest.skipIf(os.name == 'nt', 'Unix raw-client disconnect fixture')
    def test_revoke_disconnect_reconciles_only_the_original_request(self):
        self.import_evidence()
        _, status = self.serve()
        request_id = str(uuid.uuid4())
        command = dict(op='revoke', request_id=request_id, reason='rollback')
        hello = dict(abi='desktop-v1', configuration=status['configuration'], op='request', request=command)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as reader, closing(sqlite3.connect(database)) as observer:
            reader.execute('BEGIN')
            reader.execute('SELECT generation FROM service').fetchone()
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(str(self.desktop / 'service.sock'))
                client.sendall(json.dumps(hello).encode() + b'\n')
                # Pin actual durable admission, keeping checkpoint blocked so
                # closing the socket loses the result after acceptance. Closing
                # before hello was read need not admit anything on macOS.
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    row = observer.execute('SELECT state FROM desktop_commands WHERE request_id=?', (request_id,)).fetchone()
                    if row == ('prepared',):
                        break
                    time.sleep(0.01)
                else:
                    self.fail('revoke was not durably admitted')
            reader.execute('ROLLBACK')
        # Losing the response is not permission to invent a new request. The
        # actor finishes independently, and an exact read finds that receipt.
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            result = self.cli('desktop', 'request', '--json', data=dict(op='revoke_status', request_id=request_id))
            if result.get('checkpointed'):
                break
            time.sleep(0.02)
        else:
            self.fail('disconnected revoke did not settle')
        self.assertEqual(self.cli('desktop', 'revoke', '--json', '--request-id', request_id, '--reason', 'rollback'), result)
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_commands').fetchone()[0], 1)

    def test_revoke_refuses_newer_marker_and_newer_database_without_repair(self):
        path, _, marker = self.import_evidence()
        marker['version'] = 2
        path.write_text(json.dumps(marker))
        before = path.read_bytes()
        refused = self.cli('desktop', 'revoke', '--json', ok=False)
        self.assertEqual(json.loads(refused.stdout)['code'], 'unsupported_handoff')
        self.assertEqual(path.read_bytes(), before)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            db.execute('PRAGMA user_version=999')
        before = database.read_bytes()
        self.cli('desktop', 'revoke', '--json', ok=False)
        self.assertEqual(database.read_bytes(), before)

    @unittest.skipIf(os.name == 'nt', 'POSIX owner-lock contention fixture')
    def test_offline_revoke_cannot_bypass_a_live_or_unreachable_owner(self):
        import fcntl
        self.desktop.mkdir(parents=True, mode=0o700)
        self.state.chmod(0o700)
        lock = self.desktop / 'owner.lock'
        with lock.open('w') as owner:
            lock.chmod(0o600)
            fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
            refused = self.cli('desktop', 'revoke', '--json', ok=False)
            self.assertIn('another desktop service', refused.stderr)
            self.assertFalse((self.desktop / 'state.sqlite3').exists())
        self.assertTrue(self.cli('desktop', 'revoke', '--json')['revoked'])

    @unittest.skipIf(os.name == 'nt', 'POSIX handoff-lock contention fixture')
    def test_live_revoke_refuses_a_busy_handoff_without_acceptance(self):
        import fcntl
        self.import_evidence()
        self.serve()
        path = self.desktop / 'handoff.lock'
        with path.open('r+') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            refused = self.cli('desktop', 'revoke', '--json', ok=False)
            response = json.loads(refused.stdout)
            self.assertFalse(response['accepted'])
            self.assertFalse(response['revoked'])
            self.assertEqual(response['code'], 'handoff_unavailable')
        self.assertTrue(self.cli('desktop', 'revoke', '--json')['revoked'])

    def test_revoke_strict_validation_and_status_never_admit_an_effect(self):
        self.serve()
        for args in [[], ['--json', '--json'], ['--json', '--request-id', 'bad'],
                     ['--json', '--reason', 'automatic'], ['--json', '--enabled']]:
            self.cli('desktop', 'revoke', *args, ok=False)
        for op in ('revoke', 'revoke_status'):
            value = dict(op=op, request_id=str(uuid.uuid4()), enabled=True)
            result = self.cli('desktop', 'request', '--json', data=value)
            self.assertFalse(result['accepted'])
            self.assertEqual(result['code'], 'invalid_revoke_request')
        result = self.cli('desktop', 'request', '--json', data=dict(op='revoke_status', request_id=str(uuid.uuid4())))
        self.assertEqual(result['code'], 'request_not_found')
        self.assertFalse(self.cli('desktop', 'status', '--json')['writers_enabled'])

    def test_service_restart_fences_prior_effects_and_keeps_manual_revocation(self):
        process, _ = self.serve()
        self.stop(process)
        database = self.desktop / 'state.sqlite3'
        with closing(sqlite3.connect(database)) as db, db:
            db.execute("INSERT INTO desktop_intents VALUES(?,'{}',1,'{}',0,0)", ('a' * 64,))
            db.execute("INSERT INTO desktop_attempts(request_id,fingerprint,intent_id,revision,epoch,service_epoch,adapter,lease,state) VALUES(?,?,?,1,?,?,?,?,'reserved')",
                       (str(uuid.uuid4()), 'b' * 64, 'a' * 64, *[str(uuid.uuid4()) for _ in range(4)]))
            db.execute("UPDATE desktop_writer SET reason='manual_rollback',revoked=1")
        _, status = self.serve()
        self.assertTrue(status['effect_journal']['revoked'])
        self.assertEqual(status['effect_journal']['reason'], 'manual_rollback')
        self.assertEqual(status['effect_journal']['attempts']['unknown'], 1)
        self.assertEqual(status['effect_journal']['attempts']['reserved'], 0)
        self.assertFalse(status['writers_enabled'])
        self.assertFalse((self.root / 'unexpected-tool').exists())

    @unittest.skipIf(os.name == 'nt', 'Unix socket and mode checks')
    def test_unsafe_files_and_socket_targets_are_preserved(self):
        self.desktop.mkdir(mode=0o700, parents=True)
        self.state.chmod(0o700)
        foreign = self.root / 'foreign'
        foreign.write_bytes(b'preserve these bytes')
        for name in ('owner.lock', 'state.sqlite3', 'state.sqlite3-wal', 'service.sock'):
            leaf = self.desktop / name
            leaf.symlink_to(foreign)
            self.failed_serve()
            self.assertTrue(leaf.is_symlink(), name)
            self.assertEqual(foreign.read_bytes(), b'preserve these bytes')
            leaf.unlink()
        leaf = self.desktop / 'owner.lock'
        if leaf.exists():
            leaf.unlink()
        os.mkfifo(leaf, 0o600)
        self.failed_serve()
        leaf.unlink()
        os.link(foreign, leaf)
        self.failed_serve()
        leaf.unlink()
        self.desktop.chmod(0o755)
        self.failed_serve()
        self.assertEqual(self.desktop.stat().st_mode & 0o777, 0o755)

    @unittest.skipIf(os.name == 'nt', 'Unix socket transport')
    def test_truncated_oversize_and_mismatched_ipc_clients_are_refused(self):
        service, status = self.serve()
        for label, payload in (
                ('oversize', b'{' + b'x' * 65536),
                ('truncated', b'{}'),
                ('abi_mismatch', b'{"abi":"desktop-v0"}\n'),
                ('configuration_mismatch', json.dumps(dict(abi='desktop-v1', configuration='wrong', op='status')).encode() + b'\n')):
            with self.subTest(payload=label), socket.socket(socket.AF_UNIX) as client:
                client.settimeout(4)
                client.connect(str(self.desktop / 'service.sock'))
                try:
                    client.sendall(payload)
                    try:
                        client.shutdown(socket.SHUT_WR)
                    except OSError as error:
                        # macOS may report a closed peer before half-close;
                        # still read and validate any buffered error response.
                        if error.errno != errno.ENOTCONN:
                            raise
                    response = client.recv(4096)
                except ConnectionResetError:
                    response = b''
                if response:
                    self.assertEqual(json.loads(response)['t'], 'error')
        self.assertIsNone(service.poll())
        self.assertEqual(self.cli('desktop', 'status', '--json')['epoch'], status['epoch'])
        result = subprocess.run([str(HGS), 'desktop', 'request', '--json'], env=self.env,
                                input='x' * 65537, text=True, capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('exceeds its limit', result.stderr)
