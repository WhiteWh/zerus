#!/usr/bin/env python3
"""Message transport tests. Private tmux socket + scripted raw-terminal agent only."""
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest
import uuid

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "target/debug/hgs")).resolve()
TMUX = shutil.which("tmux")

# Fixture agents redraw from Python signal handlers, which run between
# bytecodes. A signal that lands after the agent last checked for one and
# before it blocks in read() waits there for the next key, so the redraw never
# comes. Each handled signal also writes a byte to the wakeup pipe, so the
# select returns and the handler runs wherever the signal landed.
SIGNAL_SAFE_READ = r'''
import os, select, signal
wake, waker = os.pipe()
os.set_blocking(waker, False)
signal.set_wakeup_fd(waker, warn_on_full_buffer=False)
def read_input(fd):
    while True:
        ready = select.select([fd, wake], [], [])[0]
        if wake in ready: os.read(wake, 4096)
        if fd in ready: return os.read(fd, 65536)
'''

FAKE_AGENT = SIGNAL_SAFE_READ + r'''
import json, os, pathlib, signal, subprocess, sys, termios, threading, time, tty
root = pathlib.Path(os.environ['INPUT_FIXTURE'])
fd = sys.stdin.fileno()
original = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(1, b'\x1b[?2004h\x1b[2J\x1b[H' + '\u203a '.encode())
def screen(*args):
    os.write(1, (root / 'screen').read_bytes())
signal.signal(signal.SIGUSR1, screen)
children = []
def launch(*args):
    request = json.loads((root / 'nested-launch.json').read_text())
    with (root / 'nested.log').open('ab') as log:
        children.append(subprocess.Popen(request['argv'], env=request['env'],
            stdin=subprocess.DEVNULL, stdout=log, stderr=log))
signal.signal(signal.SIGUSR2, launch)
(root / 'agent-pid').write_text(str(os.getpid()))
# zerus-iitw8: with root/'claude-composer' (its transcript path) a bracketed
# paste shows in a Claude composer and Enter records it as Claude does.
# 'claude-pasting' holds its Pasting… footer and empty composer for the
# milliseconds it names (empty: forever), and an Enter meanwhile is swallowed
# and logged to 'claude-swallowed', as Claude 2.1.322 does with pasted
# images; 'claude-swallow' ignores every Enter.
composer, pasting, processing = '', None, False
def claude(text):
    rule = '\u2500' * 40
    shown = text if '\n' not in text.strip() else '[Pasted text #1 +%d lines]' % text.strip().count('\n')
    footer = 'Pasting\u2026' if (root / 'claude-pasting').exists() else 'paste again to expand'
    os.write(1, ('\x1b[2J\x1b[H\u25cf done\r\n' + rule + '\r\n\u276f ' + shown + '\r\n' + rule + '\r\n  ' + footer
                 + '\x1b[3;%dH' % (3 + len(shown))).encode())
try:
    while True:
        value = read_input(fd)
        if not value: break
        with (root / 'received').open('ab') as file: file.write(value)
        if (root / 'claude-composer').exists():
            if pasting is not None or value.startswith(b'\x1b[200~'):
                pasting = (pasting or b'') + value
                if b'\x1b[201~' in pasting:
                    composer = pasting.split(b'\x1b[200~', 1)[1].split(b'\x1b[201~', 1)[0].decode()
                    pasting = None
                    delay = (root / 'claude-pasting').read_text().strip() if (root / 'claude-pasting').exists() else '0'
                    if delay != '0':
                        processing = True
                        claude('')
                        if delay:
                            def settled(text=composer, wait=int(delay) / 1000):
                                global processing
                                time.sleep(wait)
                                processing = False
                                (root / 'claude-pasting').unlink(missing_ok=True)
                                claude(text)
                            threading.Thread(target=settled, daemon=True).start()
                    else:
                        claude(composer)
                continue
            if value == b'\r' and processing:
                with (root / 'claude-swallowed').open('a') as file: file.write('enter while pasting\n')
                continue
            if value == b'\r' and composer.strip() and not (root / 'claude-swallow').exists():
                transcript = pathlib.Path((root / 'claude-composer').read_text())
                # Typed and stamped as Claude records it (zerus-iitw8.4).
                now = time.time()
                stamp = time.strftime('%Y-%m-%dT%H:%M:%S', time.gmtime(now)) + '.%03dZ' % int(now % 1 * 1000)
                with transcript.open('a') as file:
                    file.write(json.dumps({'type': 'user', 'promptSource': 'typed', 'timestamp': stamp,
                                           'message': {'role': 'user', 'content': composer}}) + '\n')
                composer = ''
                claude('')
                continue
        # Optionally redraw like an agent that stops its turn on Escape.
        if value == b'\x1b' and (root / 'on-escape').exists(): os.write(1, (root / 'on-escape').read_bytes())
        # Or answer scripted keys in order, one redraw each: [[key hex, output], ...].
        replies = root / 'replies'
        queue = json.loads(replies.read_text()) if replies.exists() else []
        if queue and value.hex() == queue[0][0]:
            os.write(1, queue.pop(0)[1].encode()); replies.write_text(json.dumps(queue))
finally:
    for child in children:
        if child.poll() is None: child.terminate()
        child.wait(timeout=5)
    termios.tcsetattr(fd, termios.TCSANOW, original)
'''


class InputTransport(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hgs-input-")
        self.root = Path(self.temp.name)
        self.bin = self.root / ".local/bin"
        self.bin.mkdir(parents=True)
        self.socket = self.root / "socket"
        self.state = self.root / "state"
        self.state.mkdir()
        self.env = dict(os.environ, HOME=str(self.root), INPUT_FIXTURE=str(self.root),
                        HGS_CONFIG_DIR=str(self.root / "config"), HGS_STATE_DIR=str(self.state),
                        HGS_SELF="test", HGS_PEERS="remote", HGS_TAB="0",
                        PATH=str(self.bin) + ":" + os.environ["PATH"])
        for variable in ("TMUX", "TMUX_PANE", "HGS_RUN_ID", "HGS_SESSION", "HGS_EXECUTABLE"):
            self.env.pop(variable, None)
        (self.bin / "hgs").symlink_to(HGS)
        self.script("tmux", '#!/bin/sh\nexec ' + TMUX + ' -S "' + str(self.socket) + '" -f /dev/null "$@"\n')
        (self.root / "agent.py").write_text(FAKE_AGENT)
        self.name = "codex/input-test"
        self.run_id = str(uuid.uuid4())
        self.conversation_id = str(uuid.uuid4())
        self.addCleanup(lambda: subprocess.run([TMUX, "-S", str(self.socket), "kill-server"], capture_output=True))
        self.tmux("new-session", "-d", "-s", self.name, "-x", "100", "-y", "30",
                  "python3", str(self.root / "agent.py"))
        # The agent creates the file before writing its PID; wait for the content.
        self.wait_for(lambda: (self.root / "agent-pid").exists() and (self.root / "agent-pid").read_text().strip())
        pid = int((self.root / "agent-pid").read_text())
        start = subprocess.check_output(["ps", "-p", str(pid), "-o", "lstart="],
                                       env=dict(self.env, LC_ALL="C", TZ="UTC"), text=True).strip()
        self.pane = self.tmux("display-message", "-p", "-t", "=" + self.name + ":", "#{pane_id}").strip()
        self.tmux("set-option", "-t", "=" + self.name + ":", "@hgs_run", self.run_id)
        self.record_path = self.state / (hashlib.sha256(self.name.encode()).hexdigest() + ".json")
        self.record = dict(version=1, name=self.name, agent="codex", run_id=self.run_id,
                           conversation_id=self.conversation_id, pane=self.pane, pid=pid,
                           process_start=start, activity="idle", phase="idle", active_tools={},
                           subagents={}, last_event_at=time.time(), created=time.time())
        self.write_record()

    def tearDown(self):
        pids = {pid for pid in self.tmux('list-panes', '-a', '-F', '#{pane_pid}', check=False).splitlines()
                if pid.isdigit()}
        self.tmux("kill-server", check=False)
        # kill-server acknowledges before its panes finish exiting. An
        # interactive shell can still save history into the fixture home;
        # wait for owned processes before removing that home.
        deadline = time.monotonic() + 2
        while pids:
            status = subprocess.run(['ps', '-p', ','.join(sorted(pids)), '-o', 'pid=,stat='],
                                    capture_output=True, text=True, timeout=5)
            pids = {parts[0] for line in status.stdout.splitlines()
                    if len(parts := line.split()) >= 2 and not parts[1].startswith(('Z', 'X'))}
            if not pids or time.monotonic() >= deadline:
                break
            time.sleep(.02)
        self.assertFalse(pids, 'owned fixture panes survived tmux shutdown')
        self.temp.cleanup()

    def script(self, name, value):
        path = self.bin / name
        path.write_text(value)
        path.chmod(0o755)

    def write_record(self):
        self.record_path.write_text(json.dumps(self.record))

    def tmux(self, *args, check=True):
        result = subprocess.run([str(self.bin / "tmux"), *args], env=self.env,
                                capture_output=True, text=True, timeout=5)
        if check: self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def wait_for(self, callback):
        for _ in range(100):
            if callback(): return
            time.sleep(.02)
        self.fail("fixture timed out")

    def wait_until(self, callback, timeout, describe=lambda: ""):
        """Polls `callback` until it holds or `timeout` seconds of wall clock
        pass. For an event a loaded runner reaches far later than a quiet one
        (zerus-eubyp); `describe` says what the fixture saw when it gave up."""
        deadline = time.monotonic() + timeout
        while not callback():
            if time.monotonic() >= deadline:
                self.fail(f"fixture timed out after {timeout} s: {describe()}")
            time.sleep(.02)

    def payload(self, **updates):
        value = dict(request_id=str(uuid.uuid4()), text="hello", attachments=[],
                     expected_run_id=self.run_id, expected_conversation_id=self.conversation_id)
        value.update(updates)
        return value

    def send(self, payload=None, remote=False, success=True):
        payload = payload or self.payload()
        result = subprocess.run([str(HGS), *( ["@remote"] if remote else []), "send", self.name, "--json"],
                                input=json.dumps(payload), text=True, capture_output=True,
                                env=self.env, timeout=10)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0)
        return result.stderr

    def received(self):
        path = self.root / "received"
        return path.read_bytes() if path.exists() else b""

    def claude_composer(self):
        """The fake agent shows pastes in a Claude composer and records Enter in the transcript (zerus-iitw8)."""
        if not self.record.get('transcript'):
            self.record['transcript'] = str(self.root / 'claude-transcript.jsonl')
            self.write_record()
        (self.root / 'claude-composer').write_text(self.record['transcript'])

    def composer_for(self, agent):
        if agent == 'claude':
            self.claude_composer()
        else:
            (self.root / 'claude-composer').unlink(missing_ok=True)

    def screen(self, value, cursor):
        import signal
        # Successive screens often leave the cursor in the same cell, so the
        # cursor alone can still be the previous screen's. The agent writes a
        # new title after this screen and tmux applies output in order: once
        # the pane has that title, the whole screen is drawn.
        self.frames = getattr(self, "frames", 0) + 1
        title = "input-test-screen-%d" % self.frames
        (self.root / "screen").write_bytes(value.encode() + b"\x1b]2;" + title.encode() + b"\x1b\\")
        os.kill(self.record["pid"], signal.SIGUSR1)
        self.wait_for(lambda: self.tmux("display-message", "-p", "-t", self.pane,
                                      "#{pane_title} #{cursor_x}:#{cursor_y}").strip() == title + " " + cursor)

    def fresh_unconfirmed(self):
        self.record.update(run_identity_version=1,
                           supervisor=dict(pid=self.record['pid'],start=self.record['process_start']),
                           conversation_id=None,conversation_state='unknown',activity='unknown',phase='unknown',
                           startup_kind='new')
        self.record.pop('last_event_at',None)
        self.write_record()

    def resumed_unconfirmed(self, fork_parent=None, native=None):
        # A native executable name, exact parent PID and an open rollout are
        # all checked by the bridge. No real provider or model call is involved.
        # `native` replaces the C source of that executable; it takes the same
        # arguments and writes agent-pid and received the same way.
        self.tmux('kill-session', '-t', self.name)
        (self.root / 'agent-pid').unlink()
        self.rollout = self.root / 'saved.jsonl'
        meta = dict(type='session_meta', payload=dict(id=self.conversation_id))
        if fork_parent:
            self.rollout = self.root / '.codex/sessions/rollout-fork.jsonl'
            self.rollout.parent.mkdir(parents=True)
            meta['payload'].update(forked_from_id=fork_parent, source='cli', cwd=str(self.root))
        self.rollout.write_text(json.dumps(meta)+'\n')
        source = self.root / 'codex.c'
        source.write_text(native or r'''
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <termios.h>
#include <unistd.h>
static void screen(int ignored) {
    char b[8192]; int fd=open("screen",O_RDONLY); ssize_t n;
    while(fd>=0 && (n=read(fd,b,sizeof(b)))>0) write(1,b,n);
    if(fd>=0)close(fd);
}
int main(int argc,char **argv) {
    if(argc!=3 || chdir(argv[1]) || open(argv[2],O_RDONLY)<0)return 2;
    struct termios t;tcgetattr(0,&t);cfmakeraw(&t);tcsetattr(0,TCSANOW,&t);
    signal(SIGUSR1,screen);
    printf("\033[?2004h\033[2J\033[H› ");fflush(stdout);
    FILE *f=fopen("agent-pid","w");fprintf(f,"%d",getpid());fclose(f);
    char b[65536];ssize_t n;
    while((n=read(0,b,sizeof(b)))>0) {
        int out=open("received",O_CREAT|O_APPEND|O_WRONLY,0600);
        write(out,b,n);close(out);
    }
    return 0;
}
''')
        native = self.bin / 'codex'
        subprocess.run(['cc', str(source), '-o', str(native)], check=True, capture_output=True)
        self.tmux('new-session','-d','-s',self.name,'-x','100','-y','30',str(native),str(self.root),str(self.rollout))
        self.wait_for(lambda:(self.root/'agent-pid').exists() and (self.root/'agent-pid').read_text().strip())
        pid = int((self.root/'agent-pid').read_text())
        parent = int(subprocess.check_output(['ps','-p',str(pid),'-o','ppid='],text=True).strip())
        def start(p):
            return subprocess.check_output(['ps','-p',str(p),'-o','lstart='],env=dict(self.env,LC_ALL='C',TZ='UTC'),text=True).strip()
        self.pane=self.tmux('display-message','-p','-t','='+self.name+':','#{pane_id}').strip()
        self.tmux('set-option','-t','='+self.name+':','@hgs_run',self.run_id)
        self.record.update(run_identity_version=1,startup_kind='resume',pid=pid,process_start=start(pid),
            supervisor=dict(pid=parent,start=start(parent)),expected_id=self.conversation_id,
            transcript=str(self.rollout),activity='unknown',phase='unknown',pane=self.pane,last_event_at=0)
        if fork_parent:
            self.record.update(startup_kind='new', conversation_id=None, transcript=None,
                               fork_parent_id=fork_parent, launch_dir=str(self.root),
                               base=['codex', 'fork', fork_parent])
            self.record.pop('expected_id')
        self.write_record()

    def test_native_fork_accepts_first_message_before_deferred_sessionstart(self):
        parent = str(uuid.uuid4())
        parent_history = self.root / 'parent.jsonl'
        parent_history.write_text(json.dumps(dict(id=parent, context=['Original context']))+'\n')
        original = parent_history.read_bytes()
        self.resumed_unconfirmed(fork_parent=parent)
        info = self.inspect()
        self.assertTrue(info['first_message_can_send'], info.get('first_message_reason'))
        self.assertFalse(info.get('conversation_id'))
        self.assertFalse(json.loads(self.record_path.read_text()).get('conversation_id'))
        attachment = dict(name='plot.png', mime='image/png', reference='[Image #1]',
                          data_base64=base64.b64encode(b'fixture image').decode())
        payload = self.payload(expected_conversation_id='', text='[Image #1] hello', attachments=[attachment])
        receipt = self.send(payload)
        self.assertTrue(receipt['first_message'])
        self.wait_for(lambda:self.received().endswith(b'\r'))
        received = self.received()
        self.assertIn(b'[Image #1]', received)
        self.assertFalse(self.inspect()['first_message_can_send'])
        self.assertEqual(self.send(payload), receipt)
        self.assertEqual(self.received(), received)
        self.send(self.payload(expected_conversation_id=''), success=False)
        self.assertEqual(self.received(), received)
        self.assertEqual(parent_history.read_bytes(), original)
        # Only the child's native event confirms the conversation; the bridge
        # never manufactures SessionStart from a history file.
        event = dict(hook_event_name='SessionStart', session_id=self.conversation_id,
                     transcript_path=str(self.rollout))
        result = subprocess.run([str(HGS), '__state', 'hook'], env=dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id),
                                input=json.dumps(event), capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        confirmed = self.inspect()
        self.assertEqual(confirmed['conversation_id'], self.conversation_id)
        self.assertEqual(confirmed['fork_parent_id'], parent)
        self.assertNotEqual(confirmed['conversation_id'], parent)
        self.assertEqual(parent_history.read_bytes(), original)
        self.assertIn('identity changed', self.send(self.payload(expected_conversation_id=''), success=False))
        self.assertEqual(self.received(), received)

    def test_fork_bootstrap_requires_exact_native_child_history_and_process(self):
        parent = str(uuid.uuid4())
        self.resumed_unconfirmed(fork_parent=parent)
        initial = self.record.copy()
        patches = [dict(fork_parent_id=str(uuid.uuid4())), dict(agent='claude'),
                   dict(base=['codex', 'resume', parent]), dict(base=['codex', 'fork', str(uuid.uuid4())]),
                   dict(expected_id=self.conversation_id), dict(requested_id=self.conversation_id),
                   dict(agent_home=str(self.root/'other-account')), dict(process_start='old process'),
                   dict(supervisor=dict(pid=os.getpid(), start=initial['supervisor']['start'])),
                   dict(last_event_at=time.time()), dict(input_pending_at=time.time()),
                   dict(error='wrong conversation')]
        for patch in patches:
            with self.subTest(patch=patch):
                self.record = dict(initial, **patch); self.write_record()
                self.assertFalse(self.inspect().get('first_message_can_send', False))
                self.send(self.payload(expected_conversation_id=''), success=False)
                self.assertEqual(self.received(), b'')
        self.record = initial; self.write_record()
        meta = json.loads(self.rollout.read_text())
        for patch in [dict(id=parent), dict(id='invalid'), dict(forked_from_id=str(uuid.uuid4())),
                      dict(source='exec'), dict(cwd=str(self.root.parent)),
                      dict(base_instructions='x'*(256*1024))]:
            with self.subTest(metadata=next(iter(patch))):
                changed = dict(meta, payload=dict(meta['payload'], **patch))
                self.rollout.write_text(json.dumps(changed)+'\n')
                self.assertFalse(self.inspect()['first_message_can_send'])
                self.send(self.payload(expected_conversation_id=''), success=False)
                self.assertEqual(self.received(), b'')
        self.rollout.write_text(json.dumps(meta)+'\n')
        # Native -C/--cd launch options are preserved by fork recipes. Resolve
        # relative paths from the launch directory when checking child history.
        workspace = self.root/'workspace'; workspace.mkdir()
        changed = dict(meta, payload=dict(meta['payload'], cwd=str(workspace)))
        self.rollout.write_text(json.dumps(changed)+'\n')
        for flags in [['-C', 'workspace'], ['--cd=workspace']]:
            self.record = dict(initial, base=['codex', 'fork', parent, *flags]); self.write_record()
            self.assertTrue(self.inspect()['first_message_can_send'])
        self.record = initial; self.write_record()
        # A valid unopened neighbor cannot replace this process's own history.
        (self.rollout.parent/'rollout-unowned.jsonl').write_text(json.dumps(meta)+'\n')
        self.rollout.write_text(json.dumps(dict(meta, payload=dict(meta['payload'], id=parent)))+'\n')
        self.assertFalse(self.inspect()['first_message_can_send'])
        self.send(self.payload(expected_conversation_id=''), success=False)
        self.assertEqual(self.received(), b'')
        self.rollout.write_text(json.dumps(meta)+'\n')
        self.screen('\x1b[2J\x1b[HPassword: ', '10:0')
        self.assertFalse(self.inspect()['first_message_can_send'])
        self.send(self.payload(expected_conversation_id=''), success=False)
        self.assertEqual(self.received(), b'')
        self.screen('\x1b[2J\x1b[H› ', '2:0')
        self.assertTrue(self.inspect()['first_message_can_send'])
        # Owning an identical header outside this account's native session store
        # is insufficient, even when the file is still held by the same process.
        self.rollout.rename(self.root/'rollout-outside.jsonl')
        self.assertFalse(self.inspect()['first_message_can_send'])
        self.send(self.payload(expected_conversation_id=''), success=False)
        self.assertEqual(self.received(), b'')

    def test_send_now_uses_native_queue_shortcuts_without_repasting(self):
        self.record.update(run_identity_version=1, supervisor=dict(pid=self.record['pid'], start=self.record['process_start']),
                           activity='busy', phase='working')
        samples = [
            ('claude', '❯ queued message\r\nctrl+x ctrl+s to send now\r\n\r\n❯ ', '2:3', b'\x18\x13'),
            ('kimi', '──────────────────────────────\r\n  ❯ queued message\r\n  ↑ to edit · ctrl-s to steer immediately\r\n ╭──────────────────╮\r\n │ >                │\r\n ╰──────────────────╯\x1b[5;6H', '5:4', b'\x13'),
            ('codex', '• Messages to be submitted after next tool call\r\n  (press esc to interrupt and send immediately)\r\n  ↳ queued message\r\n\r\n› ', '2:4', b'\x1b'),
        ]
        for provider, screen, cursor, keys in samples:
            with self.subTest(provider=provider):
                self.record['agent'] = provider; self.write_record()
                self.screen('\x1b[2J\x1b[H' + screen, cursor)
                queue = self.inspect().get('input_queue')
                self.assertIsNotNone(queue)
                self.assertTrue(queue['can_send_now'], queue)
                payload = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                               expected_conversation_id=self.conversation_id, queue_id=queue['id'])
                def send(value):
                    return subprocess.run([str(HGS), 'send-now', self.name, '--json'], input=json.dumps(value),
                                          text=True, capture_output=True, env=self.env, timeout=10)
                before = self.received()
                rejected = send(dict(payload, queue_id='stale'))
                self.assertNotEqual(rejected.returncode, 0); self.assertEqual(self.received(), before)
                result = send(payload); self.assertEqual(result.returncode, 0, result.stderr)
                self.wait_for(lambda: len(self.received()) == len(before)+len(keys))
                self.assertEqual(self.received(), before+keys)
                self.assertNotEqual(send(payload).returncode, 0)
                self.assertEqual(self.received(), before+keys)
                # Queue still visible but another draft must never go with it.
                self.screen('\x1b[2J\x1b[H' + screen + 'draft', str(int(cursor.split(':')[0])+5)+':'+cursor.split(':')[1])
                self.assertFalse(self.inspect()['input_queue']['can_send_now'])
                self.assertNotEqual(send(dict(payload, request_id=str(uuid.uuid4()))).returncode, 0)
                self.assertEqual(self.received(), before+keys)

    def test_resumed_codex_can_send_before_deferred_session_start(self):
        self.resumed_unconfirmed()
        info=self.inspect()
        self.assertTrue(info['resume_message_can_send'])
        self.assertEqual(info['phase'],'idle')
        self.assertEqual(info['expected_id'],self.conversation_id)
        self.assertFalse(info['fork_supported'])
        self.assertEqual(info['events'],[])
        receipt=self.send()
        self.assertEqual(receipt['conversation_id'],self.conversation_id)
        self.assertFalse(receipt['first_message'])
        self.wait_for(lambda:self.received().endswith(b'\r'))
        before=self.received()
        self.assertFalse(self.inspect()['resume_message_can_send'])
        self.send(success=False)
        self.assertEqual(self.received(),before)
        # Only the provider hook clears the expected identity.
        self.assertEqual(json.loads(self.record_path.read_text())['expected_id'],self.conversation_id)
        env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=self.run_id)
        event=dict(hook_event_name='SessionStart',session_id=self.conversation_id,transcript_path=str(self.rollout))
        hook=subprocess.run([str(HGS),'__state','hook'],env=env,input=json.dumps(event),capture_output=True,text=True)
        self.assertEqual(hook.returncode,0,hook.stderr)
        self.assertNotIn('expected_id',self.inspect())

    def test_resumed_codex_can_pause_before_deferred_session_start(self):
        self.resumed_unconfirmed()
        self.record.update(base=['codex'], launch_dir=str(self.root)); self.write_record()
        initial = self.record.copy()
        pause = lambda: subprocess.run([str(HGS), 'pause', self.name], env=self.env, capture_output=True, text=True, timeout=20)
        # A sent message may have started a turn, an archive restore commits only
        # on its hook, and a draft or dialog is not the verified empty composer.
        for patch, screen in [(dict(input_pending_at=time.time()), None),
                              (dict(restore_archive_id=str(uuid.uuid4())), None),
                              ({}, ('\x1b[2J\x1b[H› unsent draft', '14:0')),
                              ({}, ('\x1b[2J\x1b[HTrust this folder? ', '19:0'))]:
            with self.subTest(patch=patch, screen=screen):
                self.record = dict(initial, **patch); self.write_record()
                self.screen(*(screen or ('\x1b[2J\x1b[H› ', '2:0')))
                self.assertNotIn('pause', self.inspect()['allowed_actions'])
                self.assertNotEqual(pause().returncode, 0)
                self.assertTrue(self.tmux('list-panes', '-t', '=' + self.name, check=False))
        self.record = initial; self.write_record(); self.screen('\x1b[2J\x1b[H› ', '2:0')
        self.assertIn('pause', self.inspect()['allowed_actions'])
        result = pause()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f'hgs: paused {self.name}\n')
        paused = json.loads(self.record_path.read_text())
        self.assertTrue(paused['paused'])
        # Resume restores the same saved conversation; nothing confirmed or typed.
        self.assertEqual((paused['conversation_id'], paused['expected_id']), (self.conversation_id, self.conversation_id))
        self.assertFalse(paused.get('error'))
        self.assertEqual(self.received(), b'')

    def test_resume_input_rejects_wrong_history_process_dialog_and_stale_run(self):
        self.resumed_unconfirmed(); initial=self.record.copy()
        other=self.root/'other.jsonl';other.write_bytes(self.rollout.read_bytes())
        for patch in [dict(transcript=str(other)),dict(expected_id=str(uuid.uuid4())),
                      dict(process_start='old process'),dict(run_id='replaced'),
                      dict(supervisor=dict(pid=os.getpid(),start=initial['supervisor']['start'])),
                      dict(activity='busy',phase='tool'),dict(error='wrong conversation'),
                      dict(last_event_at=time.time()),dict(input_pending_at=time.time())]:
            with self.subTest(patch=patch):
                self.record=dict(initial,**patch);self.write_record()
                self.assertFalse(self.inspect().get('resume_message_can_send',False))
                self.send(success=False);self.assertEqual(self.received(),b'')
        self.record=initial;self.write_record()
        self.rollout.write_text(json.dumps(dict(type='session_meta',payload=dict(id='different')))+'\n')
        self.assertFalse(self.inspect()['resume_message_can_send']);self.send(success=False)
        self.rollout.write_text(json.dumps(dict(type='session_meta',payload=dict(id=self.conversation_id)))+'\n')
        self.screen('\x1b[2J\x1b[HTrust this folder? ','19:0')
        self.assertFalse(self.inspect()['resume_message_can_send']);self.send(success=False)
        self.assertEqual(self.received(),b'')

    def inspect(self):
        r=subprocess.run([str(HGS),'inspect',self.name],env=self.env,capture_output=True,text=True,timeout=10)
        self.assertEqual(r.returncode,0,r.stderr)
        return json.loads(r.stdout)

    def test_first_message_with_image_starts_lazy_conversation(self):
        self.fresh_unconfirmed()
        info=self.inspect()
        self.assertTrue(info['first_message_can_send'])
        self.assertIsNone(info['conversation_id'])
        data=b'fixture image'
        payload=self.payload(expected_conversation_id='',attachments=[dict(name='screenshot.png',mime='image/png',data_base64=base64.b64encode(data).decode())])
        receipt=self.send(payload)
        self.assertTrue(receipt['first_message'])
        self.assertEqual(receipt['conversation_id'],'')
        self.assertEqual(receipt['run_id'],self.run_id)
        self.assertEqual(Path(receipt['attachments'][0]['path']).read_bytes(),data)
        self.wait_for(lambda:self.received().endswith(b'\r'))
        before=self.received();self.assertEqual(self.send(payload),receipt);self.assertEqual(self.received(),before)
        self.assertIsNone(json.loads(self.record_path.read_text())['conversation_id'])
        self.assertFalse(self.inspect()['first_message_can_send'])
        self.send(self.payload(expected_conversation_id=''),success=False)
        self.assertEqual(self.received(),before)
        # The real provider emits SessionStart only after receiving that first turn.
        event=dict(hook_event_name='SessionStart',session_id=self.conversation_id)
        env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=self.run_id)
        hook=subprocess.run([str(HGS),'__state','hook'],env=env,input=json.dumps(event),capture_output=True,text=True,timeout=10)
        self.assertEqual(hook.returncode,0,hook.stderr)
        confirmed=self.inspect()
        self.assertEqual(confirmed['conversation_id'],self.conversation_id)
        self.assertFalse(confirmed['first_message_can_send'])
        # A stale initial-composer request cannot follow a newly confirmed ID.
        self.assertIn('identity changed',self.send(self.payload(expected_conversation_id=''),success=False))
        self.assertEqual(self.received(),before)

    def test_mixed_provider_hooks_preserve_parent_and_scope_activity(self):
        for parent,provider,sid,path in [('codex','kimi','session_'+str(uuid.uuid4()),'/tmp/wire.jsonl'),
                                         ('kimi','codex',str(uuid.uuid4()),'/tmp/rollout-child.jsonl')]:
            self.record.update(agent=parent,model='parent-model',conversation_id=self.conversation_id,
                               main_done=False,active_tools={},subagents={},subagent_groups={})
            self.write_record()
            env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=self.run_id)
            for kind in ['SessionStart','PreToolUse','PostToolUse','Stop','SessionEnd']:
                event=dict(hook_event_name=kind,session_id=sid,transcript_path=path,cwd='/tmp',
                           model='child-model',tool_name='Bash',last_assistant_message='Only child response')
                done=subprocess.run([str(HGS),'__state','hook'],env=env,input=json.dumps(event),capture_output=True,text=True,timeout=10)
                self.assertEqual(done.returncode,0,done.stderr)
            current=self.inspect()
            self.assertEqual(current['conversation_id'],self.conversation_id)
            self.assertEqual(current['model'],'parent-model')
            children=current['subagents'];self.assertEqual(len(children),1)
            child_id,child=next(iter(children.items()));self.assertEqual(child['provider'],provider)
            self.assertEqual(child['state'],'finished')
            raw=json.loads(self.record_path.read_text());self.assertFalse(raw['main_done'])
            result=subprocess.run([str(HGS),'inspect',self.name,'--agent',child_id],env=self.env,capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr)
            child_view=json.loads(result.stdout);self.assertEqual(child_view['provider'],provider)
            self.assertTrue(child_view['read_only']);self.assertTrue(any(e.get('detail')=='Only child response' for e in child_view['events']))
            self.assertTrue(all(not e.get('agent_id') for e in child_view['events']))

    def test_ephemeral_codex_hooks_use_verified_caller_and_preserve_parent(self):
        import signal
        self.script('codex', '''#!/usr/bin/env python3
import json, os, pathlib, subprocess
root = pathlib.Path(os.environ['INPUT_FIXTURE'])
for kind in ['SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'Stop', 'SessionEnd']:
    event = dict(hook_event_name=kind, session_id=os.environ['CHILD_CONVERSATION'],
        cwd=str(root), model='child-model', tool_name='Bash',
        tool_use_id='child-tool', last_assistant_message='Child review result')
    if kind == 'UserPromptSubmit': event['prompt'] = 'Child review request'
    subprocess.run(['hgs', '__state', 'hook'], input=json.dumps(event), text=True, check=True)
(root / 'nested-done').write_text('done')
''')
        for parent in ('claude', 'codex'):
            with self.subTest(parent=parent):
                sid = str(uuid.uuid4())
                self.record.update(agent=parent, model='parent-model', transcript='parent-history',
                    cwd=str(self.root), prompt='Parent request', main_done=False,
                    active_tools={'parent-tool': {'name':'Bash'}}, subagents={})
                self.write_record()
                env = dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id,
                           CHILD_CONVERSATION=sid, CODEX_HOME=str(self.root / '.codex'))
                (self.root / 'nested-done').unlink(missing_ok=True)
                (self.root / 'nested-launch.json').write_text(json.dumps(dict(
                    argv=[str(self.bin / 'codex'), 'exec', '--ephemeral'], env=env)))
                os.kill(self.record['pid'], signal.SIGUSR2)
                self.wait_for(lambda: (self.root / 'nested-done').exists())
                raw = json.loads(self.record_path.read_text())
                for key in ('conversation_id', 'run_id', 'pid', 'process_start', 'model',
                            'transcript', 'cwd', 'prompt', 'main_done', 'active_tools'):
                    self.assertEqual(raw[key], self.record[key], key)
                child_id, child = next(iter(raw['subagents'].items()))
                self.assertEqual(child['conversation_id'], sid)
                self.assertEqual(child['provider'], 'codex')
                self.assertEqual(child['state'], 'finished')
                result = subprocess.run([str(HGS), 'inspect', self.name, '--agent', child_id],
                    env=self.env, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                view = json.loads(result.stdout)
                self.assertTrue(view['read_only'])
                self.assertTrue(any(e.get('type') == 'UserPromptSubmit' and
                    e.get('detail') == 'Child review request' for e in view['events']))
                self.assertTrue(any(e.get('detail') == 'Child review result' for e in view['events']))
                self.assertEqual(self.received(), b'')

    def test_killed_external_child_without_stop_is_not_working_and_can_be_retired(self):
        import signal
        self.script('codex', '''#!/usr/bin/env python3
import json, os, pathlib, signal, subprocess
root = pathlib.Path(os.environ['INPUT_FIXTURE'])
for kind in ['SessionStart', 'UserPromptSubmit']:
    event = dict(hook_event_name=kind, session_id=os.environ['CHILD_CONVERSATION'], cwd=str(root))
    subprocess.run(['hgs', '__state', 'hook'], input=json.dumps(event), text=True, check=True)
def pending(*args):
    event = dict(hook_event_name='PermissionRequest', session_id=os.environ['CHILD_CONVERSATION'], tool_name='Bash', tool_use_id='pending', tool_input=dict(command='fixture'))
    subprocess.run(['hgs', '__state', 'hook'], input=json.dumps(event), text=True, check=True)
    (root / 'child-hook-finished').write_text('finished')
signal.signal(signal.SIGUSR1, pending)
def progressed(*args):
    event = dict(hook_event_name='PostToolUse', session_id=os.environ['CHILD_CONVERSATION'], tool_name='Bash', tool_use_id='pending')
    subprocess.run(['hgs', '__state', 'hook'], input=json.dumps(event), text=True, check=True)
    (root / 'child-hook-progressed').write_text('finished')
signal.signal(signal.SIGUSR2, progressed)
(root / 'child-pid').write_text(str(os.getpid()))
while True: signal.pause()
''')
        sid = str(uuid.uuid4())
        self.record.update(agent='claude', main_done=True, activity='idle', phase='idle', active_tools={})
        self.write_record()
        env = dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id,
                   CHILD_CONVERSATION=sid, CODEX_HOME=str(self.root / '.codex'))
        launch = dict(argv=[str(self.bin / 'codex'), 'exec', '--ephemeral'], env=env)
        (self.root / 'nested-launch.json').write_text(json.dumps(launch))
        os.kill(self.record['pid'], signal.SIGUSR2)
        self.wait_for(lambda: (self.root / 'child-pid').exists() and (self.root / 'child-pid').read_text().strip())
        pid = int((self.root / 'child-pid').read_text())
        before = self.inspect()
        self.assertEqual(before['subagent_active_count'], 1)
        child_id, child = next(iter(before['subagents'].items()))
        raw = json.loads(self.record_path.read_text())
        self.assertEqual(raw['subagents'][child_id]['original_process']['pid'], pid)
        self.assertTrue(raw['subagents'][child_id]['original_process']['birth'])
        request = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id, agent_id=child_id,
                       expected_updated=child['updated'])
        def retire(payload):
            return subprocess.run([str(HGS), 'subagent', 'retire', self.name, '--json'],
                                  input=json.dumps(payload), env=self.env, text=True,
                                  capture_output=True, timeout=5)
        preserved = self.record_path.read_bytes()
        refused = retire(request)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('still live', refused.stderr)
        self.assertEqual(self.record_path.read_bytes(), preserved)
        # Model a legacy/unbound record: explicit retirement can mark it Ended,
        # but a genuine hook with fresh process evidence must recover it without
        # SessionStart. A different recorded birth must not be adopted that way.
        raw['subagents'][child_id].pop('original_process')
        self.record_path.write_text(json.dumps(raw))
        self.assertEqual(retire(request).returncode, 0)
        retired = json.loads(self.record_path.read_text())
        retired['subagents'][child_id]['original_process'] = dict(
            json.loads(preserved)['subagents'][child_id]['original_process'], birth='different original birth')
        self.record_path.write_text(json.dumps(retired))
        wrong_birth = self.record_path.read_bytes()
        os.kill(pid, signal.SIGUSR1)
        self.wait_for(lambda: (self.root / 'child-hook-finished').exists())
        self.assertEqual(self.record_path.read_bytes(), wrong_birth)
        (self.root / 'child-hook-finished').unlink()
        retired['subagents'][child_id].pop('original_process')
        self.record_path.write_text(json.dumps(retired))
        os.kill(pid, signal.SIGUSR1)
        self.wait_for(lambda: (self.root / 'child-hook-finished').exists())
        resumed = self.inspect()['subagents'][child_id]
        self.assertEqual(resumed['display_state'], 'approval')
        self.assertEqual(resumed['attention_id'], 'pending')
        self.assertEqual(json.loads(self.record_path.read_text())['subagents'][child_id]['original_process']['pid'], pid)
        self.assertEqual(retire(request).returncode, 0, 'original receipt must not retire the recovered child')
        request = dict(request, request_id=str(uuid.uuid4()), expected_updated=resumed['updated'])
        # Finish the explicit attention before testing loss of Working. Unknown
        # process evidence must not hide an outstanding approval or input.
        os.kill(pid, signal.SIGUSR2)
        self.wait_for(lambda: (self.root / 'child-hook-progressed').exists())
        self.assertIsNone(self.inspect()['subagents'][child_id].get('display_state'))
        from test_watch import Stream
        stream = Stream(self)
        snapshot = stream.next(lambda frame: frame['t'] == 'snapshot' and frame['topic'] == 'catalog' and frame['last'])
        listed = next(item['row'] for item in snapshot['items'] if item['row'].get('name') == self.name)
        self.assertEqual(listed['subagent_active_count'], 1)
        unchanged = self.record_path.read_bytes()
        # Kill ONLY the private fake child, with no Stop/SessionEnd hook.
        os.kill(pid, signal.SIGKILL)
        delta = stream.next(lambda frame: frame['t'] == 'delta' and frame['topic'] == 'catalog' and any(
            item['row'].get('name') == self.name and item['row'].get('subagent_active_count') == 0
            for item in frame['upsert']))
        self.assertTrue(delta['upsert'])
        self.assertEqual(self.record_path.read_bytes(), unchanged, 'watch must observe exit without a new hook or write')
        self.assertEqual(stream.close(), 0)
        self.wait_for(lambda: self.inspect()['subagent_active_count'] == 0)
        ended = self.inspect()
        self.assertEqual(ended['activity'], 'idle')
        self.assertIn(ended['subagents'][child_id].get('display_state'), ('ended', 'unknown'))
        request['expected_updated'] = ended['subagents'][child_id]['updated']
        result = retire(request)
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads(result.stdout)
        self.assertEqual(receipt['state'], 'ended')
        self.assertIs(receipt['native_completion'], False)
        preserved = self.record_path.read_bytes()
        again = retire(request)
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(json.loads(again.stdout), receipt)
        self.assertEqual(self.record_path.read_bytes(), preserved)
        # A fresh, genuinely observed SessionStart can resume this conversation;
        # replaying the old retirement only reads its receipt, never re-retires it.
        (self.root / 'child-pid').unlink()
        os.kill(self.record['pid'], signal.SIGUSR2)
        self.wait_for(lambda: (self.root / 'child-pid').exists() and (self.root / 'child-pid').read_text().strip())
        self.assertEqual(self.inspect()['subagent_active_count'], 1)
        self.assertEqual(retire(request).returncode, 0)
        self.assertEqual(self.inspect()['subagent_active_count'], 1)
        view = subprocess.run([str(HGS), 'inspect', self.name, '--agent', child_id],
                              env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(view.returncode, 0, view.stderr)
        self.assertFalse(any(event.get('type') in ('Stop', 'SubagentStop') for event in json.loads(view.stdout)['events']))

    def test_unknown_foreign_session_start_cannot_rebind_parent(self):
        env = dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id)
        event = dict(hook_event_name='SessionStart', session_id=str(uuid.uuid4()), model='foreign-model')
        result = subprocess.run([str(HGS), '__state', 'hook'], env=env,
            input=json.dumps(event), capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(self.record_path.read_text()), self.record)

    def test_subagent_retirement_checks_exact_guards_and_finite_input_and_record(self):
        self.record.update(agent='claude', main_done=True, activity='idle', phase='idle', active_tools={},
                           subagents={'stale': dict(external=True, state='working', updated=2.0)})
        self.write_record()
        request = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id, agent_id='stale', expected_updated=2.0)
        def refused(payload, reason):
            before = self.record_path.read_bytes()
            result = subprocess.run([str(HGS), 'subagent', 'retire', self.name, '--json'],
                                    input=json.dumps(payload), env=self.env, text=True,
                                    capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(reason, result.stderr)
            self.assertEqual(self.record_path.read_bytes(), before)
        refused(dict(request, expected_run_id='changed'), 'generation changed')
        refused(dict(request, expected_conversation_id='changed'), 'generation changed')
        refused(dict(request, agent_id='missing'), 'not in the selected conversation')
        refused(dict(request, expected_updated=3.0), 'changed; inspect it again')
        refused(dict(request, extra=True), 'Invalid subagent retirement request')
        refused(dict(request, extra='x' * 8192), 'exceeds 8 KiB')
        self.record['subagents']['stale']['group'] = True
        self.write_record()
        refused(request, 'profile group')
        self.record['retirement_padding'] = 'x' * (16 * 1024 * 1024)
        self.write_record()
        refused(request, 'exceeds retirement bound')

    def test_login_delay_cannot_receive_first_prompt_until_native_composer(self):
        self.fresh_unconfirmed()
        self.screen('\x1b[2J\x1b[HPassword: ','10:0')
        self.assertFalse(self.inspect()['first_message_can_send'])
        self.assertIn('draft',self.send(self.payload(expected_conversation_id=''),success=False))
        self.assertEqual(self.received(),b'')
        self.screen('\x1b[2J\x1b[H› ','2:0')
        self.assertTrue(self.inspect()['first_message_can_send'])
        self.send(self.payload(expected_conversation_id=''))
        self.wait_for(lambda:self.received().endswith(b'\r'))

    def test_bootstrap_never_bypasses_stale_or_requested_identity(self):
        self.fresh_unconfirmed();initial=self.record.copy()
        cases=[dict(expected_id='saved-conversation'),dict(requested_id='saved-conversation'),
               dict(startup_kind='resume'),dict(fork_parent_id='source-conversation'),
               dict(last_event_at=time.time()),dict(run_identity_version=0),dict(supervisor=None),
               dict(error='provider resumed wrong conversation'),dict(phase='approval'),
               dict(process_start='reused process'),dict(run_id='replaced run')]
        for patch in cases:
            with self.subTest(patch=patch):
                self.record=dict(initial,**patch);self.write_record()
                self.assertFalse(self.inspect().get('first_message_can_send',False))
                self.send(self.payload(expected_conversation_id=''),success=False)
                self.assertEqual(self.received(),b'')

    def test_initial_model_is_observed_from_this_terminal_not_default_config(self):
        self.fresh_unconfirmed()
        codex=self.root/'.codex';codex.mkdir()
        (codex/'models_cache.json').write_text(json.dumps(dict(models=[dict(slug='observed-model',display_name='Observed model',supported_reasoning_levels=[dict(effort='high')]),dict(slug='default-model',display_name='Default model',supported_reasoning_levels=[dict(effort='low')])])))
        (codex/'config.toml').write_text('model="default-model"\nmodel_reasoning_effort="low"\n')
        # Native model footer is authoritative; no provider API or input is used.
        self.screen('\x1b[2J\x1b[27;1H\x1b[48;5;234m› '+ ' '*80 +'\x1b[28;1H   \x1b[49m\x1b[29;1H  Observed model high · /fixture\x1b[30;1H  ? for shortcuts\x1b[27;3H','2:26')
        info=self.inspect()
        self.assertEqual(info['model'],'observed-model');self.assertEqual(info['effort'],'high')
        self.assertEqual(self.received(),b'')
        self.assertFalse(info['settings_change_supported']) # no invented conversation binding
        self.screen('\x1b[0m\x1b[2J\x1b[HPassword: ','10:0')
        self.assertEqual(self.inspect()['model'],'')

    def test_terminal_draft_and_missing_bracketed_paste_mode_are_refused(self):
        self.screen("\x1b[2J\x1b[H› existing draft", "16:0")
        self.assertIn("draft", self.send(success=False))
        self.screen("\x1b[2J\x1b[H› \x1b[?2004l", "2:0")
        self.assertIn("input mode", self.send(success=False))
        self.assertEqual(self.received(), b"")

    # zerus-ucck P3b: input goes to the session's own terminal, named apart
    # from its label as a resume beside an untracked terminal names it; the
    # input claim checks that terminal's name, never the label's.
    def test_send_reaches_a_terminal_named_apart_from_its_label(self):
        own = self.name + "~3f2a9c1d"
        self.tmux("rename-session", "-t", "=" + self.name, own)
        self.record["terminal_name"] = own
        self.write_record()
        self.screen("\x1b[2J\x1b[H› \x1b[?2004h", "2:0")
        self.send()
        self.wait_for(lambda: self.received().endswith(b"\r"))

    def test_empty_claude_prompt_is_supported(self):
        self.record["agent"] = "claude"
        self.write_record()
        self.claude_composer()
        self.screen("\x1b[2J\x1b[H❯ ", "2:0")
        self.send()
        self.wait_for(lambda: self.received().endswith(b"\r"))

    def claude_ready(self):
        self.record["agent"] = "claude"
        self.write_record()
        self.claude_composer()
        self.screen("\x1b[2J\x1b[H❯ ", "2:0")

    def slow_send(self, payload):
        """hgs send with room for the whole settle and proof window (8 s)."""
        return subprocess.run([str(HGS), "send", self.name, "--json"], input=json.dumps(payload), text=True,
                              capture_output=True, env=self.env, timeout=30)

    def receipt_file(self, payload):
        return json.loads((self.state / "input_receipts" / (payload["request_id"] + ".json")).read_text())

    def claude_rows(self):
        path = Path(self.record["transcript"])
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def test_claude_enters_once_its_pasted_images_settle_and_proves_it(self):
        # zerus-iitw8: Claude shows an empty composer with Pasting… while it
        # processes pasted image paths and swallows an Enter meanwhile.
        self.claude_ready()
        (self.root / "claude-pasting").write_text("900")
        payload = self.payload(text="Report:\n/srv/peer/target/shot.png\nDone.")
        receipt = self.send(payload)
        self.assertEqual((receipt["status"], receipt["native"]), ("submitted", "transcript"))
        self.assertFalse((self.root / "claude-swallowed").exists(), "Enter was pressed while Claude was pasting")
        self.assertEqual(self.received().count(b"\r"), 1)
        self.assertEqual([row["message"]["content"] for row in self.claude_rows()], [receipt["submitted_text"]])
        self.assertEqual(self.send(payload), receipt)
        self.assertEqual(self.received().count(b"\r"), 1)

    def test_claude_paste_that_never_settles_is_uncertain_without_an_enter(self):
        self.claude_ready()
        (self.root / "claude-pasting").write_text("")
        payload = self.payload(text="Report with an image:\n/srv/peer/target/shot.png")
        result = self.slow_send(payload)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("delivery uncertain: Claude's composer never showed the pasted message settled; no Enter was pressed", result.stderr)
        self.assertNotIn(b"\r", self.received())
        receipt = self.receipt_file(payload)
        self.assertEqual((receipt["status"], receipt["delivery"]), ("in_progress", "uncertain"))
        self.assertIsNone(receipt["composer"])
        # Never offered again, never NotOffered.
        before = self.received()
        replay = self.slow_send(payload)
        self.assertIn("do not resend automatically", replay.stderr)
        self.assertEqual(self.received(), before)

    def test_a_swallowed_claude_enter_is_uncertain_with_the_composer_residue(self):
        self.claude_ready()
        (self.root / "claude-swallow").write_text("")
        text = "Peer report\nwith two lines"
        payload = self.payload(text=text)
        result = self.slow_send(payload)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("delivery uncertain: Claude recorded no submission of this message; the composer holds: [Pasted text #1 +1 lines]", result.stderr)
        receipt = self.receipt_file(payload)
        self.assertEqual((receipt["status"], receipt["delivery"], receipt["composer"]), ("in_progress", "uncertain", "[Pasted text #1 +1 lines]"))
        self.assertEqual(self.received().count(b"\r"), 1, "exactly one Enter, never a blind second one")
        self.assertEqual(self.claude_rows(), [])
        # The unconfirmed input, and the residue behind it, keep every later message out.
        self.assertIn("previous input has not been confirmed", self.send(self.payload(text="next"), success=False))
        self.assertEqual(self.received().count(b"\x1b[200~"), 1)

    def test_a_busy_agents_tool_events_do_not_confirm_the_previous_input(self):
        self.record.update(activity="busy", phase="tool", active_tools={"one": {}})
        self.write_record()
        self.screen("\x1b[2J\x1b[H› ", "2:0")
        self.send(self.payload(text="first queued message"))
        def hook(kind):
            event = dict(hook_event_name=kind, session_id=self.conversation_id, tool_name="Bash", tool_use_id="t2")
            result = subprocess.run([str(HGS), "__state", "hook"], env=dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id),
                                    input=json.dumps(event), capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
        hook("PreToolUse")
        hook("PostToolUse")
        self.screen("\x1b[2J\x1b[H› ", "2:0")
        refused = self.send(self.payload(text="second"), success=False)
        self.assertIn("previous input has not been confirmed", refused)
        # zerus-iitw8.6: a busy agent may hold it in its own queue until it takes it.
        self.assertIn("a busy agent may keep it queued until it accepts it", refused)
        hook("UserPromptSubmit")
        self.screen("\x1b[2J\x1b[H› ", "2:0")
        self.assertEqual(self.send(self.payload(text="second"))["status"], "submitted")

    def background_send(self, payload):
        """hgs send running while the test acts; communicate() collects it."""
        proc = subprocess.Popen([str(HGS), "send", self.name, "--json"], stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=self.env)
        self.addCleanup(lambda: proc.poll() is None and proc.kill())
        proc.stdin.write(json.dumps(payload))
        proc.stdin.close()
        return proc

    def state_lock_free(self):
        import fcntl
        with (self.state / ".lock").open("rb") as file:
            try:
                fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return False
            fcntl.flock(file, fcntl.LOCK_UN)
            return True

    def claude_hook(self, kind):
        event = dict(hook_event_name=kind, session_id=self.conversation_id)
        result = subprocess.run([str(HGS), "__state", "hook"], env=dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id),
                                input=json.dumps(event), capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)

    def terminal_request(self, **values):
        payload = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id, **values)
        result = subprocess.run([str(HGS), "terminal", self.name, "--json"], input=json.dumps(payload), text=True,
                                capture_output=True, env=self.env, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_a_settling_claude_paste_holds_no_lock_and_keeps_other_input_out(self):
        # zerus-iitw8.5: the settle wait holds no state lock, so hooks run
        # meanwhile; this send's marker keeps every other input out even when
        # an acceptance event (here an earlier queued prompt taken now) opens
        # the unconfirmed-input gate.
        self.claude_ready()
        (self.root / "claude-pasting").write_text("3000")
        proc = self.background_send(self.payload(text="First message"))
        self.wait_for(lambda: b"\x1b[201~" in self.received())
        self.wait_until(self.state_lock_free, 2, lambda: "the settle holds the state lock")
        self.assertTrue((self.root / "claude-pasting").exists(), "Claude is still pasting")
        self.claude_hook("UserPromptSubmit")
        self.assertTrue((self.root / "claude-pasting").exists(), "the hook ran during the settle")
        record = json.loads(self.record_path.read_text())
        self.assertGreater(record["input_seen_at"], record["input_pending_at"], "the gate alone would let a send in")
        self.assertIn("another message is being entered", self.send(self.payload(text="Second message"), success=False))
        # Terminal input and Clear input check the same marker.
        binding = self.terminal_request(action="snapshot")["terminal_binding_id"]
        typed = self.terminal_request(action="input", terminal_binding_id=binding, expires_at=time.time() + 5, text="typed", enter=False)
        self.assertEqual(typed["status"], "failed")
        self.assertIn("A message is being entered", typed["error"])
        self.assertTrue((self.root / "claude-pasting").exists(), "all of it ran during the settle")
        self.assertEqual(self.received().count(b"\x1b[200~"), 1)
        out, err = proc.communicate(timeout=30)
        self.assertEqual(proc.returncode, 0, err)
        receipt = json.loads(out)
        self.assertEqual((receipt["status"], receipt["native"]), ("submitted", "transcript"))
        self.assertEqual(self.received().count(b"\r"), 1)
        self.assertNotIn("input_typing", json.loads(self.record_path.read_text()))

    def enter_after_a_change_under_the_lock(self, change):
        """Holds the state lock while the paste settles, applies `change`, then
        lets the send take the lock for its Enter."""
        import fcntl
        self.claude_ready()
        (self.root / "claude-pasting").write_text("500")
        payload = self.payload(text="Settled message")
        proc = self.background_send(payload)
        self.wait_for(lambda: b"\x1b[201~" in self.received())
        with (self.state / ".lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.wait_until(lambda: not (self.root / "claude-pasting").exists(), 5)
            # The settled composer holds for its stable time; the send then waits here.
            time.sleep(.6)
            self.assertNotIn(b"\r", self.received())
            change()
        out, err = proc.communicate(timeout=30)
        return proc, err, payload

    def test_claude_enter_rechecks_the_composer_under_the_lock(self):
        # zerus-iitw8.5: the composer changed between the settle and the lock.
        proc, err, payload = self.enter_after_a_change_under_the_lock(lambda: self.screen("\x1b[2J\x1b[H❯ ", "2:0"))
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("delivery uncertain: Claude's composer never showed the pasted message settled; no Enter was pressed", err)
        self.assertNotIn(b"\r", self.received())
        self.assertEqual(self.claude_rows(), [])
        record = json.loads(self.record_path.read_text())
        self.assertNotIn("input_typing", record)
        self.assertEqual(self.receipt_file(payload)["delivery"], "uncertain")

    def test_claude_enter_needs_its_own_marker_under_the_lock(self):
        # zerus-iitw8.5: without this send's marker other input may have typed.
        def expire():
            self.record = json.loads(self.record_path.read_text())
            self.record.pop("input_typing")
            self.write_record()
        proc, err, payload = self.enter_after_a_change_under_the_lock(expire)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("delivery uncertain: the session changed while the message settled; no Enter was pressed", err)
        self.assertNotIn(b"\r", self.received())
        self.assertEqual(self.receipt_file(payload)["delivery"], "uncertain")

    def test_claude_presses_no_enter_when_the_lock_outlasts_the_window(self):
        # zerus-iitw8.5: the Enter waits for the lock only within the same window.
        proc, err, payload = self.enter_after_a_change_under_the_lock(lambda: time.sleep(8))
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("delivery uncertain: the state lock stayed busy until the window ended; no Enter was pressed", err)
        self.assertNotIn(b"\r", self.received())
        self.assertEqual(self.receipt_file(payload)["delivery"], "uncertain")
        self.assertNotIn("input_typing", json.loads(self.record_path.read_text()))

    def test_a_claude_proof_confirms_only_its_own_pending_input(self):
        # zerus-iitw8.6: a later pending input (here written while this send
        # waits for its proof) is never confirmed by this send's proof.
        self.claude_ready()
        (self.root / "claude-swallow").write_text("")
        payload = self.payload(text="First message")
        proc = self.background_send(payload)
        self.wait_until(lambda: self.received().endswith(b"\r"), 5)
        self.record = json.loads(self.record_path.read_text())
        seen = self.record["input_seen_at"]
        later = self.record["input_pending_at"] + 0.001
        self.record["input_pending_at"] = later
        self.write_record()
        now = time.time()
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(now)) + ".%03dZ" % int(now % 1 * 1000)
        with Path(self.record["transcript"]).open("a") as file:
            file.write(json.dumps({"type": "user", "promptSource": "typed", "timestamp": stamp,
                                   "message": {"role": "user", "content": "First message"}}) + "\n")
        out, err = proc.communicate(timeout=30)
        self.assertEqual(proc.returncode, 0, err)
        self.assertEqual(json.loads(out)["native"], "transcript")
        record = json.loads(self.record_path.read_text())
        self.assertEqual(record["input_pending_at"], later)
        self.assertEqual(record["input_seen_at"], seen, "the later input stays unconfirmed")
        self.assertNotIn("input_typing", record)

    def test_a_claude_transcript_proof_confirms_the_input_without_its_hook(self):
        # zerus-iitw8.6: this fixture runs no UserPromptSubmit hook.
        self.claude_ready()
        first = self.send(self.payload(text="First message"))
        self.assertEqual((first["status"], first["native"]), ("submitted", "transcript"))
        record = json.loads(self.record_path.read_text())
        self.assertGreater(record["input_seen_at"], record["input_pending_at"])
        self.assertNotIn("input_typing", record)
        second = self.send(self.payload(text="Second message"))
        self.assertEqual((second["status"], second["native"]), ("submitted", "transcript"))
        self.assertEqual([row["message"]["content"] for row in self.claude_rows()], ["First message", "Second message"])

    def test_a_lost_claude_enter_is_never_proven_by_another_or_an_earlier_message(self):
        # zerus-iitw8.4: this send's Enter is lost; meanwhile Claude records an
        # earlier queued "continue" it takes now, a longer typed message that
        # contains this one, and a row stamped before the paste.
        self.claude_ready()
        (self.root / "claude-swallow").write_text("")
        payload = self.payload(text="continue")
        proc = self.background_send(payload)
        self.wait_until(lambda: self.received().endswith(b"\r"), 5)
        earlier = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(time.time() - 60))
        now = time.strftime("%Y-%m-%dT%H:%M:%S.999Z", time.gmtime())
        rows = [
            {"type": "queue-operation", "operation": "remove", "content": "continue", "commandUuid": "c1", "reason": "absorbed_mid_turn", "timestamp": now},
            {"type": "attachment", "timestamp": now, "attachment": {"type": "queued_command", "prompt": "continue", "source_uuid": "c1"}},
            {"type": "queue-operation", "operation": "dequeue", "timestamp": now},
            {"type": "user", "promptSource": "queued", "timestamp": now, "message": {"role": "user", "content": "continue"}},
            {"type": "user", "promptSource": "typed", "timestamp": now, "message": {"role": "user", "content": "Please review the PR.\ncontinue"}},
            {"type": "user", "promptSource": "typed", "timestamp": earlier, "message": {"role": "user", "content": "continue"}},
        ]
        with Path(self.record["transcript"]).open("a") as file:
            file.write("".join(json.dumps(row) + "\n" for row in rows))
        out, err = proc.communicate(timeout=30)
        self.assertNotEqual(proc.returncode, 0, out)
        self.assertIn("delivery uncertain: Claude recorded no submission of this message", err)
        receipt = self.receipt_file(payload)
        self.assertEqual((receipt["status"], receipt["delivery"]), ("in_progress", "uncertain"))
        self.assertEqual(self.received().count(b"\r"), 1)

    def test_claude_nbsp_prompt_accepts_docx_without_overwriting_drafts(self):
        self.record['agent']='claude';self.write_record()
        self.screen('\x1b[2J\x1b[H❯\u00a0existing\r\n──────────────\x1b[1;3H','2:0')
        self.assertIn('draft',self.send(success=False));self.assertEqual(self.received(),b'')
        self.screen('\x1b[2J\x1b[H❯\u00a0\r\n──────────────\r\n auto mode on\x1b[1;3H','2:0')
        self.claude_composer()
        data=b'fake docx test content'
        receipt=self.send(self.payload(text='[File #1] Изучи документ',attachments=[dict(name='review.docx',
            mime='application/vnd.openxmlformats-officedocument.wordprocessingml.document',
            reference='[File #1]',data_base64=base64.b64encode(data).decode())]))
        self.wait_for(lambda:self.received().endswith(b'\r'))
        self.assertIn('Изучи документ'.encode(),self.received());self.assertIn(b'review.docx',self.received())

    def test_multiline_draft_is_refused_when_cursor_returns_to_empty_first_line(self):
        for agent, prompt in (("codex", "›"), ("claude", "❯")):
            self.record["agent"] = agent
            self.write_record()
            self.screen(f"\x1b[2J\x1b[H{prompt} \r\n  existing second line\x1b[1;3H", "2:0")
            self.wait_for(lambda: "existing second line" in self.tmux("capture-pane", "-p", "-t", self.pane))
            self.assertIn("draft", self.send(success=False))
            self.assertEqual(self.received(), b"")
        self.record["agent"] = "kimi"
        self.write_record()
        self.screen("\x1b[2J\x1b[H ╭──────────────────────╮\r\n │ >                    │\r\n │   existing second line│\r\n ╰──────────────────────╯\x1b[2;6H", "5:1")
        self.assertIn("draft", self.send(success=False))
        self.assertEqual(self.received(), b"")

    def test_kimi_vertical_bar_draft_is_not_mistaken_for_border(self):
        self.record["agent"] = "kimi"
        self.write_record()
        self.screen("\x1b[2J\x1b[H ╭──────────────╮\r\n │ > │          │\r\n ╰──────────────╯\x1b[2;6H", "5:1")
        self.assertIn("draft", self.send(success=False))
        self.assertEqual(self.received(), b"")

    def test_kimi_boxed_agent_prompt_and_shell_mode(self):
        self.record["agent"] = "kimi"
        self.write_record()
        self.screen("\x1b[2J\x1b[H ╭──────────────╮\r\n │ !          │\r\n ╰──────────────╯\x1b[2;6H", "5:1")
        self.send(success=False)
        self.assertEqual(self.received(), b"")
        self.screen("\x1b[2J\x1b[H ╭──────────────╮\r\n │ >          │\r\n ╰──────────────╯\x1b[2;6H", "5:1")
        # Cursor coordinates did not change; wait until the updated glyph is visible.
        self.wait_for(lambda: "│ >" in self.tmux("capture-pane", "-p", "-t", self.pane))
        self.send()
        self.wait_for(lambda: self.received().endswith(b"\r"))

    def test_unicode_multiline_is_one_literal_paste_and_one_submission(self):
        text = 'Привет\nsecond line\t$(touch /tmp/no-hgs-input-test) `literal`'
        receipt = self.send(self.payload(text=text))
        self.wait_for(lambda: self.received().endswith(b"\r"))
        self.assertEqual(self.received(), b"\x1b[200~" + text.encode() + b"\x1b[201~\r")
        self.assertEqual(receipt["status"], "submitted")
        self.assertEqual(receipt["submitted_text"], text)
        self.assertFalse(Path("/tmp/no-hgs-input-test").exists())
        self.assertEqual(self.tmux("list-buffers", "-F", "#{buffer_name}"), "")

    def test_trailing_backslash_gets_a_pasted_newline_where_enter_continues(self):
        # Claude and Kimi read backslash-Enter as line continuation; Codex submits.
        prompts = dict(claude=("\x1b[2J\x1b[H❯ ", "2:0"), codex=("\x1b[2J\x1b[H› ", "2:0"),
                       kimi=("\x1b[2J\x1b[H ╭──────────────╮\r\n │ >          │\r\n ╰──────────────╯\x1b[2;6H", "5:1"))
        text = 'Путь C:\\temp\\'
        for agent, (screen, cursor) in prompts.items():
            with self.subTest(agent=agent):
                (self.root / "received").unlink(missing_ok=True)
                self.record["agent"] = agent
                self.write_record()
                self.composer_for(agent)
                self.screen(screen, cursor)
                receipt = self.send(self.payload(text=text))
                self.wait_for(lambda: self.received().endswith(b"\r"))
                final = b"" if agent == "codex" else b"\n"
                self.assertEqual(self.received(), b"\x1b[200~" + text.encode() + final + b"\x1b[201~\r")
                self.assertEqual(receipt["submitted_text"], text)

    def test_trailing_codex_at_mention_cannot_consume_the_submit_key(self):
        prompts = dict(codex=("\x1b[2J\x1b[H› ", "2:0"), claude=("\x1b[2J\x1b[H❯ ", "2:0"),
                       kimi=("\x1b[2J\x1b[H ╭──────────────╮\r\n │ >          │\r\n ╰──────────────╯\x1b[2;6H", "5:1"))
        for agent, (screen, cursor) in prompts.items():
            for text, completion in [('Send a test to @reviewer', True), ('Review @', True),
                                     ('Первая строка\nОтветь @рецензент', True),
                                     ('Use @reviewer in this sentence.', False),
                                     ('Email reviewer@example.com', False), ('Read "@reviewer"', False)]:
                with self.subTest(agent=agent, text=text):
                    (self.root / 'received').unlink(missing_ok=True)
                    self.record['agent'] = agent
                    self.write_record()
                    self.composer_for(agent)
                    self.screen(screen, cursor)
                    payload = self.payload(text=text)
                    receipt = self.send(payload)
                    self.wait_for(lambda: self.received().endswith(b'\r'))
                    suffix = b'\n' if agent == 'codex' and completion else b''
                    expected = b'\x1b[200~' + text.encode() + suffix + b'\x1b[201~\r'
                    self.assertEqual(self.received(), expected)
                    self.assertEqual(receipt['submitted_text'], text)
                    self.assertEqual(self.send(payload), receipt)
                    self.assertEqual(self.received(), expected)

    def test_attachments_transferred_privately_and_referenced_on_agent_host(self):
        data = b"\x89PNG\r\n\x1a\n" + bytes(range(256))
        payload = self.payload(attachments=[dict(name="../../screen shot.png", mime="image/png",
                                             data_base64=base64.b64encode(data).decode())])
        receipt = self.send(payload)
        path = Path(receipt["attachments"][0]["path"])
        self.assertEqual(path.read_bytes(), data)
        self.assertTrue(path.resolve().is_relative_to((self.state / "attachments").resolve()))
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(path.parent.stat().st_mode & 0o777, 0o700)
        self.assertIn("Inspect the attached image at", receipt["submitted_text"])
        self.assertIn(str(path), receipt["submitted_text"])

    def test_attachment_history_fetch_and_conversation_binding(self):
        data=b"downloaded attachment"
        payload=self.payload(text="",attachments=[dict(name="file.txt",mime="text/plain",data_base64=base64.b64encode(data).decode())])
        receipt=self.send(payload)
        def fetch(conversation=None):
            return subprocess.run([str(HGS),"attachment",self.name,"--request",receipt["request_id"],"--index","0","--conversation",conversation or self.conversation_id],env=self.env,capture_output=True,text=True,timeout=5)
        result=fetch();self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(base64.b64decode(json.loads(result.stdout)["data_base64"]),data)
        self.assertNotIn("path",json.loads(result.stdout))
        inspected=json.loads(subprocess.check_output([str(HGS),"inspect",self.name],env=self.env,text=True))
        message=inspected["attachment_messages"][0]
        self.assertEqual(message["detail"],"");self.assertEqual(message["attachments"][0]["request_id"],receipt["request_id"])
        self.assertNotEqual(fetch("another conversation").returncode,0)
        self.record["conversation_id"]="another";self.write_record()
        self.assertNotEqual(fetch("another").returncode,0)
        self.record["conversation_id"]=self.conversation_id;self.write_record()
        Path(receipt["attachments"][0]["path"]).unlink();self.assertNotEqual(fetch().returncode,0)

    def test_inline_attachment_labels_preserve_order_on_terminal_and_in_history(self):
        text = 'До [Image #2]\nмежду [Image #1] после; ещё раз [Image #2]'
        files = [dict(name='same.png', mime='image/png', reference=f'[Image #{i}]',
                      data_base64=base64.b64encode(f'file-{i}'.encode()).decode()) for i in (1, 2)]
        receipt = self.send(self.payload(text=text, attachments=files))
        paths = [f['path'] for f in receipt['attachments']]
        expected = f'До [Image #2] (Inspect the attached image at {json.dumps(paths[1])})\nмежду [Image #1] (Inspect the attached image at {json.dumps(paths[0])}) после; ещё раз [Image #2]'
        self.assertEqual(receipt['submitted_text'], expected)
        self.wait_for(lambda: self.received().endswith(b'\r'))
        self.assertEqual(self.received(), b'\x1b[200~' + expected.encode() + b'\x1b[201~\r')
        inspected = json.loads(subprocess.check_output([str(HGS), 'inspect', self.name], env=self.env, text=True))
        saved = inspected['attachment_messages'][0]
        self.assertEqual(saved['detail'], text)
        self.assertEqual([f['reference'] for f in saved['attachments']], ['[Image #1]', '[Image #2]'])

    def test_duplicate_or_invalid_attachment_labels_are_rejected_before_input(self):
        file = dict(name='same.png', mime='image/png', reference='[Image #1]', data_base64=base64.b64encode(b'image').decode())
        for files in ([file, file], [dict(file, reference='\x1b[201~')], [dict(file, reference='text')]):
            self.assertIn('attachment reference', self.send(self.payload(attachments=files), success=False))
        self.assertEqual(self.received(), b'')

    def test_kimi_native_process_can_own_a_shell_named_pane(self):
        self.record["agent"]="kimi";self.write_record()
        real_ps=shutil.which("ps")
        self.script("ps", "#!/usr/bin/env python3\nimport subprocess,sys\nr=subprocess.run("+repr([real_ps])+"+sys.argv[1:],capture_output=True,text=True)\nout=r.stdout\nif 'tty=,pgid=,tpgid=,comm=' in sys.argv: out=' '.join(out.split()[:3])+ ' kimi\\n'\nsys.stdout.write(out);sys.exit(r.returncode)\n")
        self.script("tmux","#!/usr/bin/env python3\nimport subprocess,sys\nr=subprocess.run("+repr([TMUX,"-S",str(self.socket),"-f","/dev/null"])+"+sys.argv[1:],capture_output=True)\nout=r.stdout\nif 'display-message' in sys.argv and b'\\t' in out:\n f=out.split(b'\\t')\n if len(f)==9:f[4]=b'bash';out=b'\\t'.join(f)\nsys.stdout.buffer.write(out);sys.stderr.buffer.write(r.stderr);sys.exit(r.returncode)\n")
        self.screen("\x1b[2J\x1b[H ╭──────────────╮\r\n │ >          │\r\n ╰──────────────╯\x1b[2;6H", "5:1")
        self.send();self.wait_for(lambda:self.received().endswith(b"\r"))
        before=self.received();self.record["agent"]="codex";self.write_record()
        self.assertIn("expected agent input mode",self.send(success=False));self.assertEqual(self.received(),before)

    def test_remote_stdin_payload_never_becomes_shell_or_argv(self):
        self.script("ssh", """#!/usr/bin/env python3
import json, os, pathlib, sys
pathlib.Path(os.environ['INPUT_FIXTURE'], 'ssh-args').write_text(json.dumps(sys.argv[1:]))
os.execv('/bin/sh', ['sh', '-c', sys.argv[-1]])
""")
        payload = self.payload(text="remote secret\nsecond line", attachments=[dict(name="doc.txt",
                    mime="text/plain", data_base64=base64.b64encode(b"remote bytes").decode())])
        receipt = self.send(payload, remote=True)
        args = json.loads((self.root / "ssh-args").read_text())
        self.assertNotIn("-t", args)
        self.assertNotIn("remote secret", " ".join(args))
        self.assertEqual(args[-1], "~/.local/bin/hgs send codex/input-test --json")
        self.assertEqual(Path(receipt["attachments"][0]["path"]).read_bytes(), b"remote bytes")
        self.wait_for(lambda: self.received().endswith(b"\r"))

    def test_duplicate_request_is_acknowledged_without_resending(self):
        payload = self.payload()
        first = self.send(payload)
        self.wait_for(lambda: self.received().endswith(b"\r"))
        before = self.received()
        self.assertEqual(self.send(payload), first)
        self.assertEqual(self.received(), before)
        altered = dict(payload, text="different")
        self.assertIn("already used", self.send(altered, success=False))

    def test_graph_environment_label_cannot_claim_an_unrelated_native_process(self):
        origin = dict(self.record, name="claude/graph/source", agent="claude",
                      run_id=str(uuid.uuid4()), conversation_id=str(uuid.uuid4()))
        path = self.state / (hashlib.sha256(origin['name'].encode()).hexdigest()+'.json')
        self.env.update(HGS_SESSION=origin['name'], HGS_RUN_ID=origin['run_id'])
        # This owned but unrelated pane is not an ancestor of this hgs caller.
        path.write_text(json.dumps(origin))
        self.send(self.payload(text="A label alone cannot claim a sender"))
        ledger = self.state/'graph/sends.sqlite3'
        self.assertFalse(ledger.exists())

    def test_graph_attribution_checks_process_ancestry_and_never_replays_input(self):
        import sqlite3
        origin = dict(self.record, name="claude/graph/source", agent="claude",
                      run_id=str(uuid.uuid4()), conversation_id=str(uuid.uuid4()))
        path = self.state / (hashlib.sha256(origin['name'].encode()).hexdigest()+'.json')
        self.env.update(HGS_SESSION=origin['name'], HGS_RUN_ID=origin['run_id'])
        ledger = self.state/'graph/sends.sqlite3'
        # The fixture's actual parent is the recorded owner, just as a native
        # agent's shell tool is descended from its recorded agent process.
        origin['pid'] = os.getpid()
        origin['process_start'] = subprocess.check_output(
            ['ps','-p',str(os.getpid()),'-o','lstart='],
            env=dict(self.env,LC_ALL='C',TZ='UTC'),text=True).strip()
        path.write_text(json.dumps(origin))
        payload = self.payload(text="Verified origin content stays out of the graph")
        receipt = self.send(payload)
        self.wait_for(lambda:self.received().endswith(b'\r'))
        before = self.received()
        self.assertEqual(self.send(payload),receipt)
        self.assertEqual(self.received(),before)
        with sqlite3.connect(ledger) as db:
            events=[json.loads(row[0]) for row in db.execute('SELECT payload FROM sends')]
        self.assertEqual(len(events),1)
        self.assertEqual(events[0]['status'],'submitted')
        self.assertEqual(events[0]['source']['endpoint']['conversation'],origin['conversation_id'])
        self.assertEqual(events[0]['target'],receipt['graph_target'])
        self.assertNotIn(payload['text'],json.dumps(events))
        # Collection runs in bounded slices and reaches send stamps after the
        # bindings, so a slow runner needs several snapshots for full coverage.
        deadline=time.monotonic()+60
        while True:
            graph = subprocess.run([str(HGS),'graph','snapshot','--json'],env=self.env,
                                   capture_output=True,text=True,timeout=5)
            self.assertEqual(graph.returncode,0,graph.stderr)
            snapshot=json.loads(graph.stdout)
            if snapshot['coverage']['bindings_complete'] or time.monotonic()>deadline:
                break
        self.assertTrue(snapshot['coverage']['bindings_complete'])
        self.assertFalse(snapshot['has_more'])
        rows=snapshot['items']
        exchange=[r['item']['value'] for r in rows if r['item']['kind']=='event']
        self.assertEqual(len(exchange),1)
        self.assertEqual(exchange[0]['request_id'],payload['request_id'])
        self.assertEqual(exchange[0]['delivery_state'],'submitted')
        read=dict(name=self.name,expected_run_id=self.run_id,expected_conversation_id=self.conversation_id,
                  message_request_id=payload['request_id'])
        def evidence(value):
            return subprocess.run([str(HGS),'graph','receipt','--json'],input=json.dumps(value),env=self.env,
                                  capture_output=True,text=True,timeout=5)
        opened=evidence(read)
        self.assertEqual(opened.returncode,0,opened.stderr)
        # An agent's send arrives inside its sender envelope (tests/test_sender_envelope.py).
        detail=json.loads(opened.stdout)['event']['detail']
        self.assertTrue(detail.startswith('<zerus-message from="claude/graph/source" '),detail)
        self.assertTrue(detail.endswith('>\n'+payload['text']+'\n</zerus-message>'),detail)
        self.assertEqual(json.loads(opened.stdout)['event']['message_id'],payload['request_id'])
        for field in ('name','expected_run_id','expected_conversation_id','message_request_id'):
            value=dict(read);value[field]=str(uuid.uuid4())
            self.assertNotEqual(evidence(value).returncode,0,field)
        self.assertEqual(self.received(),before)

    def test_graph_attribution_keeps_remote_receipt_and_stdin_transport_exact(self):
        import sqlite3
        origin=dict(self.record,name='codex/graph/source',run_id=str(uuid.uuid4()),
                    conversation_id=str(uuid.uuid4()),pid=os.getpid())
        origin['process_start']=subprocess.check_output(['ps','-p',str(os.getpid()),'-o','lstart='],
            env=dict(self.env,LC_ALL='C',TZ='UTC'),text=True).strip()
        (self.state/(hashlib.sha256(origin['name'].encode()).hexdigest()+'.json')).write_text(json.dumps(origin))
        self.env.update(HGS_SESSION=origin['name'],HGS_RUN_ID=origin['run_id'])
        self.script('ssh',"""#!/usr/bin/env python3
import json,os,pathlib,sys
pathlib.Path(os.environ['INPUT_FIXTURE'],'ssh-args').write_text(json.dumps(sys.argv[1:]))
os.execv('/bin/sh',['sh','-c',sys.argv[-1]])
""")
        payload=self.payload(text='Private remote graph message')
        receipt=self.send(payload,remote=True)
        args=json.loads((self.root/'ssh-args').read_text())
        self.assertNotIn(payload['text'],' '.join(args))
        self.assertEqual(args[-1],'~/.local/bin/hgs send codex/input-test --json')
        with sqlite3.connect(self.state/'graph/sends.sqlite3') as db:
            event=json.loads(db.execute('SELECT payload FROM sends').fetchone()[0])
        self.assertEqual(event['status'],'submitted')
        self.assertEqual(event['route'],'remote')
        self.assertEqual(event['target'],receipt['graph_target'])

    def test_approval_unconfirmed_and_changed_identity_never_receive_input(self):
        cases = [dict(activity="unknown"), dict(phase="approval"), dict(phase="input"),
                 dict(conversation_id=""), dict(expected_id="pending"), dict(error="bad tracking"),
                 dict(run_id="new run"), dict(process_start="different start")]
        original = self.record.copy()
        for updates in cases:
            with self.subTest(updates=updates):
                self.record = dict(original, **updates)
                self.write_record()
                self.send(success=False)
                self.assertEqual(self.received(), b"")

    def test_busy_unknown_prompt_refuses_before_input_and_retains_only_bounded_private_diagnostics(self):
        for agent, prompt in [('codex', '›'), ('claude', '❯')]:
            self.record.update(agent=agent, activity='busy', phase='tool', active_tools={'one': {}})
            self.write_record()
            # Only the cursor row may be retained. In particular the rest of a
            # multiline draft, queued text and output must never be copied.
            for suffix in ['first', 'second']:
                self.screen('\x1b[2J\x1b[Hprivate tool output\r\n' + prompt +
                            ' cursor draft ' + suffix + '\r\n  secret continuation\r\n  secret queued message\x1b[2;3H', '2:1')
                request = self.payload(text='private attempted send')
                self.assertIn('agent busy:', self.send(request, success=False))
                self.assertEqual(self.received(), b'')
                self.assertFalse((self.state / 'input_receipts' / (request['request_id'] + '.json')).exists())
                path = self.state / 'input_diagnostics' / (agent + '.json')
                raw = path.read_bytes()
                self.assertLessEqual(len(raw), 16 * 1024)
                sample = json.loads(raw)
                self.assertIn('cursor draft ' + suffix, sample['cursor_row'])
                for text in ['secret continuation', 'secret queued message', 'private tool output', 'private attempted send']:
                    self.assertNotIn(text.encode(), raw)
                self.assertLessEqual(len(sample['rows']), 8)
                self.assertTrue(sample['cursor_stable'])
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertEqual(path.parent.stat().st_mode & 0o777, 0o700)
        self.assertEqual(sorted(p.name for p in (self.state / 'input_diagnostics').iterdir()), ['claude.json', 'codex.json'])

    def test_redraw_between_cursor_and_capture_refuses_without_input(self):
        self.record.update(activity='busy', phase='tool')
        self.write_record()
        # Capture the old empty frame, then let this fixture's native process
        # move its own cursor before the capture returns to hgs.
        (self.root / 'screen').write_text('\x1b[1;4H')
        self.script('tmux', '#!/usr/bin/env python3\n' +
            f'import os,signal,subprocess,sys,time\nargs=sys.argv[1:]\nbase={[TMUX,"-S",str(self.socket),"-f","/dev/null"]!r}\n' +
            'r=subprocess.run(base+args,stdout=subprocess.PIPE)\n' +
            'if args and args[0]=="capture-pane":\n' +
            f' os.kill({self.record["pid"]},signal.SIGUSR1)\n' +
            f' for _ in range(100):\n  if subprocess.check_output(base+["display-message","-p","-t",{self.pane!r},"#{{cursor_x}}"]).strip()==b"3":break\n  time.sleep(.01)\n' +
            'sys.stdout.buffer.write(r.stdout)\nsys.exit(r.returncode)\n')
        self.assertIn('agent busy:', self.send(success=False))
        self.assertEqual(self.received(), b'')
        sample = json.loads((self.state / 'input_diagnostics/codex.json').read_text())
        self.assertFalse(sample['cursor_stable'])
        self.assertEqual(sample['cursor_row'], '')

    def test_working_agent_accepts_next_message_without_interrupting(self):
        self.record.update(activity="busy", phase="tool", active_tools={"one": {}},
                           subagents={"one": {"state": "working"}})
        self.write_record()
        receipt = self.send(self.payload(text="next instruction"))
        self.assertEqual(receipt["status"], "submitted")
        self.wait_for(lambda: self.received().endswith(b"\r"))
        self.assertEqual(self.received(), b"\x1b[200~next instruction\x1b[201~\r")

    def provider_failure(self, native=False):
        message = 'Selected model is at capacity. Please try a different model.'
        if native:
            import sqlite3
            directory = self.root / '.codex'
            directory.mkdir()
            at = time.time() - 1
            self.record.update(activity='busy', phase='compacting', last_main_progress_at=at - 1)
            self.write_record()
            with sqlite3.connect(directory / 'logs_2.sqlite') as db:
                db.execute('CREATE TABLE logs(id INTEGER, ts INTEGER, ts_nanos INTEGER, thread_id TEXT, target TEXT, feedback_log_body TEXT)')
                db.execute('INSERT INTO logs VALUES(1,?,?,?,?,?)', (int(at), int((at % 1) * 1e9),
                           self.conversation_id, 'codex_core::session::turn', 'run_turn: Turn error: ' + message))
        else:
            self.record['agent'] = 'claude'
            self.write_record()
            self.claude_composer()
            self.screen('\x1b[2J\x1b[H❯ ', '2:0')
            event = dict(hook_event_name='StopFailure', session_id=self.conversation_id,
                         error=message, error_type='model_capacity')
            hook = subprocess.run([str(HGS), '__state', 'hook'],
                                  env=dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id),
                                  input=json.dumps(event), capture_output=True, text=True, timeout=10)
            self.assertEqual(hook.returncode, 0, hook.stderr)
            self.record = json.loads(self.record_path.read_text())

    def interrupted_turn(self):
        event = dict(hook_event_name='Interrupt', session_id=self.conversation_id)
        hook = subprocess.run([str(HGS), '__state', 'hook'],
                              env=dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id),
                              input=json.dumps(event), capture_output=True, text=True, timeout=10)
        self.assertEqual(hook.returncode, 0, hook.stderr)
        self.record = json.loads(self.record_path.read_text())

    def test_interrupted_turn_accepts_message_at_verified_prompt_without_clearing_history(self):
        self.interrupted_turn()
        before = self.record_path.read_bytes()
        info = self.inspect()
        self.assertEqual(info['phase'], 'interrupted')
        self.assertEqual(info['activity'], 'unknown')
        self.assertTrue(info.get('interrupted_message_can_send'), info.get('interrupted_message_reason'))
        self.assertEqual(self.record_path.read_bytes(), before)
        self.assertEqual(self.received(), b'')
        self.assertEqual(self.send(self.payload(text='continue'))['status'], 'submitted')
        self.wait_for(lambda: self.received().endswith(b'\r'))
        self.assertEqual(self.received(), b'\x1b[200~continue\x1b[201~\r')
        self.assertFalse(self.inspect()['interrupted_message_can_send'])
        self.send(success=False)

    def test_interrupted_turn_cannot_bypass_terminal_or_identity_checks(self):
        self.interrupted_turn()
        for screen, cursor in [('› existing draft', '16:0'), ('Password: ', '10:0'),
                               ('Working…', '8:0'), ('› \x1b[?2004l', '2:0')]:
            with self.subTest(screen=screen):
                self.screen('\x1b[2J\x1b[H' + screen, cursor)
                self.assertFalse(self.inspect().get('interrupted_message_can_send', False))
                self.send(success=False)
                self.assertEqual(self.received(), b'')
        self.screen('\x1b[?2004h\x1b[2J\x1b[H› ', '2:0')
        original = self.record.copy()
        for patch in [dict(phase='unknown'), dict(phase='approval'), dict(phase='input'),
                      dict(expected_id='pending'), dict(error='bad tracking'),
                      dict(run_id='different run'), dict(process_start='different start'),
                      dict(conversation_id=''), dict(input_pending_at=time.time()+1),
                      dict(pausing=dict(pid=original['pid'], start=original['process_start']))]:
            with self.subTest(patch=patch):
                self.record = dict(original, **patch)
                self.write_record()
                self.assertFalse(self.inspect().get('interrupted_message_can_send', False))
                self.send(success=False)
                self.assertEqual(self.received(), b'')

    def assert_provider_failure_retry(self):
        before = self.record_path.read_bytes()
        info = self.inspect()
        self.assertEqual(info['phase'], 'error')
        self.assertEqual(info['activity'], 'attention')
        self.assertEqual(info['provider_error']['error_kind'], 'capacity')
        self.assertTrue(info['error_message_can_send'], info.get('error_message_reason'))
        self.assertEqual(self.record_path.read_bytes(), before)
        receipt = self.send(self.payload(text='повтори'))
        self.assertEqual(receipt['status'], 'submitted')
        self.wait_for(lambda: self.received().endswith(b'\r'))
        self.assertEqual(self.received(), b'\x1b[200~' + 'повтори'.encode() + b'\x1b[201~\r')
        if receipt.get('native'):
            # zerus-iitw8.6: Claude's own transcript row confirms the retry even
            # without its UserPromptSubmit hook, which this fixture never runs.
            self.assertEqual(receipt['native'], 'transcript')
            record = json.loads(self.record_path.read_text())
            self.assertGreater(record['input_seen_at'], record['input_pending_at'])
            self.assertNotIn('input_typing', record)
            return
        self.assertFalse(self.inspect()['error_message_can_send'])
        self.send(success=False)

    def test_native_capacity_error_accepts_retry_without_clearing_attention(self):
        self.provider_failure(native=True)
        self.assert_provider_failure_retry()

    def test_hook_failure_accepts_retry_without_clearing_attention(self):
        self.provider_failure()
        self.assert_provider_failure_retry()

    def test_provider_failure_cannot_bypass_terminal_or_identity_checks(self):
        self.provider_failure()
        for screen, cursor in [("❯ existing draft", '16:0'), ('Password: ', '10:0'),
                               ('❯ \x1b[?2004l', '2:0')]:
            with self.subTest(screen=screen):
                self.screen('\x1b[2J\x1b[H' + screen, cursor)
                self.assertFalse(self.inspect()['error_message_can_send'])
                self.send(success=False)
                self.assertEqual(self.received(), b'')
        self.screen('\x1b[?2004h\x1b[2J\x1b[H❯ ', '2:0')
        original = self.record.copy()
        for patch in [dict(phase='approval'), dict(phase='input'), dict(expected_id='pending'),
                      dict(error='bad tracking'), dict(process_start='different start'),
                      dict(conversation_id=''),
                      dict(pausing=dict(pid=original['pid'], start=original['process_start']))]:
            with self.subTest(patch=patch):
                self.record = dict(original, **patch)
                self.write_record()
                self.assertFalse(self.inspect().get('error_message_can_send', False))
                self.send(success=False)
                self.assertEqual(self.received(), b'')

    def test_working_codex_unshaded_composer_and_custom_status_line(self):
        self.record.update(activity="busy", phase="tool", active_tools={"one": {}})
        self.write_record()
        self.screen("\x1b[0m\x1b[2J\x1b[27;1H\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m"
                    "\r\n\r\n  GPT-6-Astra xhigh · /project · Session title · Pursuing goal (1h)"
                    "\r\n  \x1b[1m?\x1b[0m for shortcuts\x1b[27;3H", "2:26")
        receipt = self.send(self.payload(text="когда это завершится, то что потом?"))
        self.assertEqual(receipt["status"], "submitted")
        self.wait_for(lambda: self.received().endswith(b"\r"))
        self.assertEqual(self.received(), b"\x1b[200~" + "когда это завершится, то что потом?".encode() + b"\x1b[201~\r")

    def test_control_sequences_slash_commands_and_invalid_attachment_are_refused(self):
        for text in ("\x1b[201~bad", "test\rnext", "\x00", " /exit", "/help args", "/clear\n/tmp/file", "!ls"):
            self.send(self.payload(text=text), success=False)
        self.send(self.payload(attachments=[dict(name="img.png", mime="image/png", data_base64="bad===")]), success=False)
        self.assertEqual(self.received(), b"")

    def test_multiple_panes_and_dead_process_are_refused(self):
        self.tmux("split-window", "-t", "=" + self.name + ":", "sleep", "30")
        self.send(success=False)
        self.assertEqual(self.received(), b"")
        self.tmux("kill-session", "-t", "=" + self.name)
        self.send(success=False)

    def test_uncertain_submission_is_never_retried(self):
        self.script("tmux", '#!/bin/sh\nif [ "$1" = send-keys ]; then exit 1; fi\nexec '
                    + TMUX + ' -S "' + str(self.socket) + '" -f /dev/null "$@"\n')
        payload = self.payload()
        self.assertIn("delivery uncertain", self.send(payload, success=False))
        self.wait_for(lambda: b"\x1b[201~" in self.received())
        first = self.received()
        self.assertIn("delivery uncertain", self.send(payload, success=False))
        self.assertEqual(self.received(), first)
        self.assertIn("previous input", self.send(success=False))

    def test_existing_attach_never_resumes_missing_or_replaced_session(self):
        wrong = subprocess.run([str(HGS), "a", self.name, "--existing", "--run-id", "wrong"],
                               env=self.env, text=True, capture_output=True, timeout=5)
        self.assertNotEqual(wrong.returncode, 0)
        self.assertIn("session run changed", wrong.stderr)
        self.tmux("kill-session", "-t", "=" + self.name)
        missing = subprocess.run([str(HGS), "a", self.name, "--existing"],
                                 env=self.env, text=True, capture_output=True, timeout=5)
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("no longer running", missing.stderr)
        self.assertEqual(self.tmux("list-sessions", check=False), "")


if __name__ == "__main__": unittest.main()
