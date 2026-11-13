#!/usr/bin/env python3
"""Kimi UserPromptSubmit hint delivery against a private pinned native image.

The interactive fixture uses a loopback model, private tmux/home/state and a
private copy with automatic updates disabled. It never uses a shared install or
an owner account. SessionStart output is separately proven inert; the model's
actual requests must contain exactly one hint at every lifecycle stage.
"""
import hashlib
import http.server
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import threading
import time
import unittest

from kimi_release import admitted_version
from test_kimi_native import Model, private_environment

KIMI = os.environ.get('HGS_KIMI_TEST_BIN')
SHA256 = os.environ.get('HGS_KIMI_TEST_SHA256', '')
HINT = ('This agent runs in a Zerus session: the hgs CLI reads, messages, launches and stops other agent '
        'sessions on this and other computers. Run hgs guide before using it.')


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def hint_count(request):
    return sum(json.dumps(message).count(HINT) for message in request.get('messages', []))


@unittest.skipUnless(KIMI, 'Set HGS_KIMI_TEST_BIN for the pinned native Kimi hint control')
class NativeKimiHint(unittest.TestCase):
    def setUp(self):
        self.version = admitted_version(KIMI, SHA256)
        self.source = Path(KIMI).resolve()
        self.source_digest = digest(self.source)
        self.temp = tempfile.TemporaryDirectory(prefix='hgs-kimi-hint-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.image = self.root / 'kimi-code'
        shutil.copyfile(self.source, self.image)
        self.image.chmod(0o500)
        self.addCleanup(self.assert_image_unchanged)
        self.home = self.root / '.kimi-code'
        self.home.mkdir()
        self.work = self.root / 'work'
        self.work.mkdir()
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
        self.server.requests, self.server.platform, self.server.platform_completed = [], [], []
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.socket = self.root / 'tmux.sock'
        self.addCleanup(lambda: subprocess.run(['tmux', '-S', str(self.socket), 'kill-server'],
                                               env=self.env, capture_output=True))
        self.env = private_environment(dict(PATH='/usr/bin:/bin'), self.root, self.home,
                                       'http://127.0.0.1:%d/kimi-platform' % self.server.server_port)
        self.env['TMUX_TMPDIR'] = str(self.root / 'tmp')
        (self.root / 'tmp').mkdir(exist_ok=True)
        (self.home / 'config.toml').write_text(
            'default_model = "fixture"\ntelemetry = false\n'
            '[providers.fixture]\ntype = "openai"\nbase_url = "http://127.0.0.1:%d/v1"\napi_key = "fixture-key"\n'
            '[models.fixture]\nprovider = "fixture"\nmodel = "fixture"\nmax_context_size = 32768\n'
            '[loop_control]\nmax_attempts_per_step = 1\n' % self.server.server_port)
        (self.home / 'tui.toml').write_text('[upgrade]\nauto_install = false\n')
        self.hook = self.root / 'hook.py'
        self.hook.write_text('''import json,sys
from pathlib import Path
event=json.load(sys.stdin)
with Path(sys.argv[1]).open('a') as stream: stream.write(json.dumps(event)+'\\n')
try: state=json.loads(Path(sys.argv[2]).read_text())
except Exception: state={}
kind=event.get('hook_event_name')
if kind == 'SessionStart' and event.get('source') == 'clear':
    state['delivered']=''
if kind == 'UserPromptSubmit':
    session=event.get('session_id') or event.get('conversation_id') or ''
    if session and state.get('delivered') != session:
        print(json.dumps({'message':sys.argv[3]}),flush=True)
        state['delivered']=session
Path(sys.argv[2]).write_text(json.dumps(state))
''')
        command = shlex.join(['/usr/bin/python3', str(self.hook), str(self.root / 'events.jsonl'),
                              str(self.root / 'state.json'), HINT])
        for event in ('SessionStart', 'UserPromptSubmit', 'PreCompact', 'PostCompact', 'Stop'):
            with (self.home / 'config.toml').open('a') as output:
                output.write('\n[[hooks]]\nevent = %s\ncommand = %s\ntimeout = 10\n' %
                             (json.dumps(event), json.dumps(command)))

    def assert_image_unchanged(self):
        self.assertEqual(digest(self.source), self.source_digest)
        self.assertEqual(digest(self.image), self.source_digest)

    def tmux(self, *args):
        result = subprocess.run(['tmux', '-S', str(self.socket), '-f', '/dev/null', *args],
                                env=self.env, cwd=self.work, text=True, capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def screen(self):
        return self.tmux('capture-pane', '-p', '-t', '=hint:')

    def wait_for(self, predicate, seconds=45):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if predicate():
                return
            time.sleep(.2)
        self.fail('Kimi hint lifecycle timed out:\n' + self.screen())

    def events(self):
        path = self.root / 'events.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def new_session_started(self):
        original = json.loads((self.root / 'state.json').read_text())['delivered']
        return any(event.get('hook_event_name') == 'SessionStart' and event.get('session_id') != original
                   for event in self.events())

    def submit(self, prompt):
        self.tmux('set-buffer', '--', prompt)
        self.tmux('paste-buffer', '-t', '=hint:')
        time.sleep(.2)
        self.tmux('send-keys', '-t', '=hint:', 'Enter')

    def test_hint_lifecycle(self):
        self.tmux('new-session', '-d', '-s', 'hint', '-x', '120', '-y', '35', str(self.image))
        self.wait_for(lambda: 'Trust this folder?' in self.screen())
        self.tmux('send-keys', '-t', '=hint:', '1', 'C-m')
        self.wait_for(lambda: 'No session yet' in self.screen())
        for prompt, expected in [
            ('first native prompt', 1),
            ('second native prompt', 2),
            ('/compact', None),
            ('after compact', 4),
            ('/clear', None),
            ('after clear', 5),
        ]:
            if prompt == 'second native prompt':
                pid = int(self.tmux('display-message', '-p', '-t', '=hint:', '#{pane_pid}'))
                self.tmux('kill-session', '-t', '=hint:')
                def exited():
                    try:
                        os.kill(pid, 0)
                    except ProcessLookupError:
                        return True
                    return False
                self.wait_for(exited)
                self.tmux('new-session', '-d', '-s', 'hint', '-x', '120', '-y', '35',
                          str(self.image), '--continue')
                self.wait_for(lambda: any(event.get('source') == 'resume' for event in self.events()))
            stopped = sum(event.get('hook_event_name') == 'Stop' for event in self.events())
            self.submit(prompt)
            if expected is not None:
                self.wait_for(lambda: len(self.server.requests) >= expected)
                self.wait_for(lambda: sum(event.get('hook_event_name') == 'Stop'
                                          for event in self.events()) > stopped)
            elif prompt == '/compact':
                self.wait_for(lambda: any(event.get('hook_event_name') == 'PostCompact'
                                          for event in self.events()))
            else:
                self.wait_for(self.new_session_started)
        evidence = dict(version=self.version, image_sha256=self.source_digest,
                        requests=self.server.requests, events=self.events())
        destination = os.environ.get('HGS_KIMI_EVIDENCE_DIR')
        if destination:
            path = Path(destination)
            path.mkdir(parents=True, exist_ok=True)
            (path / 'kimi-hint-lifecycle.json').write_text(json.dumps(evidence, indent=2))
        self.assertEqual(len(self.server.requests), 5)
        self.assertEqual([hint_count(request) for request in self.server.requests], [1, 1, 1, 1, 1])
        self.assertTrue(any(event.get('source') == 'resume' for event in self.events()))
        self.assertTrue(any(event.get('hook_event_name') == 'PostCompact' for event in self.events()))
        self.assertTrue(self.new_session_started())


if __name__ == '__main__':
    unittest.main()
