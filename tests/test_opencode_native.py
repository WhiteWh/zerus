"""Opt-in OpenCode 2 qualification: private homes, a private service port and a keyless model.

ZERUS_TEST_OPENCODE_NATIVE=1 makes a missing or unbuilt hgs, a missing OpenCode, or an
OpenCode other than 2.x at or above the reviewed minimum a failure; without it the suite
is skipped. ZERUS_TEST_OPENCODE_BIN names the OpenCode executable (default: `opencode`
on PATH); its folder leads hgs's PATH, so hgs starts that executable.

Every OpenCode and hgs command runs with private HOME and XDG directories, a private
service port and an offline catalog, so the developer's own OpenCode service, settings
and sessions are never touched. Each test stops its private service; the suite then
verifies that no process still carries its private root. No live agents, credentials or
inferencing services are used.
"""
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import random
import re
import shutil
import signal
import socket
import string
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
import uuid

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get('HGS_TEST_BIN', REPO / 'target/debug/hgs'))
NATIVE = os.environ.get('ZERUS_TEST_OPENCODE_BIN') or shutil.which('opencode') or ''
MINIMUM = (2, 0, 32)
# OpenCode's default service port; the suite never uses it.
DEFAULT_PORT = 49374
# A fixture key in the private configuration; it must never appear in hgs output.
FIXTURE_KEY = 'fixture-key-never-shown'
NOT_RUNNING = 'OpenCode is not running'
OPENCODE1 = 'This conversation belongs to OpenCode 1, which Zerus no longer supports'
REMOVED = 'Removed from the OpenCode queue before delivery'
# A configured model whose own ID contains `/`; every reader splits a value at its first `/`.
SLASH = 'fixture/vendor/slash'
# The only keys a provider row and a short-list or catalog entry may carry.
PROVIDER_KEYS = {'id', 'label', 'kind', 'credential_count', 'model_count', 'active_label', 'folder'}
ENTRY_KEYS = {'id', 'value', 'label', 'provider', 'provider_label', 'free', 'effort_options', 'effort_labels',
              'favorite', 'default'}
# The catalog reads hgs makes; it never asks OpenCode for credentials.
CATALOG_ROUTES = {'/api/info', '/openapi.json', '/api/provider', '/api/model', '/api/model/default', '/api/integration'}
FAVORITE_CONFLICT = 'This request identity belongs to another favorite change'


def free_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


def native_suffix():
    return ''.join(random.choices(string.ascii_letters + string.digits, k=26))


def owned_processes(marker):
    """(pid, command) of every other process whose command line or environment holds
    `marker`: the private root, which only processes this suite started carry."""
    found, needle = [], marker.encode()
    proc = Path('/proc')
    if proc.is_dir():
        for entry in proc.iterdir():
            if not entry.name.isdigit() or int(entry.name) == os.getpid():
                continue
            try:
                command = (entry / 'cmdline').read_bytes()
                if needle not in command and needle not in (entry / 'environ').read_bytes():
                    continue
            except OSError:
                continue
            found.append((int(entry.name), command.replace(b'\0', b' ').decode(errors='replace').strip()))
        return found
    # macOS: BSD ps shows the environment of the user's own processes with -E.
    listing = subprocess.run(['ps', '-E', '-ww', '-x', '-o', 'pid=,command='], capture_output=True,
                             text=True, errors='replace', timeout=30)
    if listing.returncode:
        raise RuntimeError('Cannot list processes: ' + listing.stderr[-500:])
    for line in listing.stdout.splitlines():
        pid, _, command = line.strip().partition(' ')
        if pid.isdigit() and int(pid) != os.getpid() and marker in command:
            found.append((int(pid), command[:300]))
    return found


class Model(http.server.BaseHTTPRequestHandler):
    """A keyless OpenAI-compatible model. A marker in the newest user message selects
    the reply: HOLD_NATIVE holds the turn open, QUESTION_NATIVE calls OpenCode 2's
    `question` tool, BASH_NATIVE its `shell` tool and ERROR_NATIVE answers HTTP 401."""

    def log_message(self, *_):
        pass

    def do_GET(self):
        body = json.dumps({'object': 'list', 'data': [{'id': 'local', 'object': 'model'},
                                                      {'id': 'alternate', 'object': 'model'}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        messages = body.get('messages', [])
        tools = [tool.get('function', {}).get('name') for tool in body.get('tools', [])]
        last_user = next((str(m.get('content', '')) for m in reversed(messages) if m.get('role') == 'user'), '')
        self.server.requests.append({'model': body.get('model'), 'tools': tools, 'last_user': last_user[:200],
                                     'options': {k: v for k, v in body.items() if k not in ('messages', 'tools')}})
        if 'ERROR_NATIVE' in last_user:
            error = json.dumps({'error': {'message': 'Fixture provider rejected', 'code': 'invalid_api_key'}}).encode()
            self.send_response(401)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(error)))
            self.end_headers()
            self.wfile.write(error)
            return
        tool = None
        # Only an agent turn offers tools; a turn that just received a tool result replies.
        if tools and not any(m.get('role') == 'tool' for m in messages[-2:]):
            if 'HOLD_NATIVE' in last_user:
                self.server.holding.set()
                self.server.release.wait(60)
            elif 'QUESTION_NATIVE' in last_user:
                tool = ('question', {'questions': [{'question': 'Pick a color', 'header': 'Color', 'options': [
                    {'label': 'Green', 'description': 'First'}, {'label': 'Blue', 'description': 'Second'}]}]})
            elif 'BASH_NATIVE' in last_user:
                tool = ('shell', {'command': 'printf native-tool-output', 'description': 'Print isolated fixture output'})
        if tool:
            call = {'index': 0, 'id': 'call_' + uuid.uuid4().hex[:12], 'type': 'function',
                    'function': {'name': tool[0], 'arguments': json.dumps(tool[1])}}
            choices = [{'index': 0, 'delta': {'tool_calls': [call]}, 'finish_reason': None},
                       {'index': 0, 'delta': {}, 'finish_reason': 'tool_calls'}]
        else:
            choices = [{'index': 0, 'delta': {'content': 'Native OpenCode reply'}, 'finish_reason': None},
                       {'index': 0, 'delta': {}, 'finish_reason': 'stop'}]
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            for choice in choices:
                chunk = {'id': 'chatcmpl-fixture', 'object': 'chat.completion.chunk', 'created': int(time.time()),
                         'model': body.get('model', 'fixture'), 'choices': [choice]}
                self.wfile.write(('data: ' + json.dumps(chunk) + '\n\n').encode())
                self.wfile.flush()
            self.wfile.write(b'data: [DONE]\n\n')
        except (BrokenPipeError, ConnectionResetError):
            pass


def stub_service(version, pid, openapi=None):
    """A loopback HTTP stub that answers like an OpenCode service of `version`."""
    class Stub(http.server.BaseHTTPRequestHandler):
        seen = []

        def log_message(self, *_):
            pass

        def answer(self, status, value):
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            Stub.seen.append(('GET', self.path))
            if self.path == '/api/info':
                self.answer(200, {'version': version, 'pid': pid, 'urls': []})
            elif self.path == '/openapi.json' and openapi is not None:
                self.answer(200, openapi)
            else:
                self.answer(404, {'message': 'Not found'})

        def other(self):
            Stub.seen.append((self.command, self.path))
            self.answer(404, {'message': 'Not found'})
        do_POST = do_PATCH = do_DELETE = do_PUT = other

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, Stub.seen


@unittest.skipUnless(os.environ.get('ZERUS_TEST_OPENCODE_NATIVE') == '1', 'explicit private native qualification required')
class OpenCodeNative(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not NATIVE or not Path(NATIVE).is_file() or not HGS.is_file():
            raise RuntimeError('The opted-in OpenCode suite requires OpenCode 2 and a built hgs')
        cls.temp = tempfile.TemporaryDirectory(prefix='zerus-opencode-native-')
        cls.root = Path(cls.temp.name)
        cls.model = None
        try:
            cls.prepare()
        except BaseException:
            cls.cleanup()
            raise

    @classmethod
    def prepare(cls):
        root = cls.root
        # Registration files this suite wrote itself, byte for byte.
        cls.fake_registrations = set()
        cls.workspace = root / 'workspace'
        cls.state = root / 'hgs-state'
        for path in ['home', 'xdg-config/opencode', 'xdg-data', 'xdg-cache', 'xdg-state', 'workspace', 'project',
                     'hgs-config', 'hgs-state']:
            (root / path).mkdir(parents=True)
        # hgs lists tmux sessions too; an existing private TMUX_TMPDIR keeps it off the
        # user's default server, where a missing directory would fall back to it.
        (root / 'tmux').mkdir(mode=0o700)
        cls.model = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
        cls.model.holding, cls.model.release, cls.model.requests = threading.Event(), threading.Event(), []
        threading.Thread(target=cls.model.serve_forever, daemon=True).start()
        native = Path(NATIVE).absolute()
        cls.env = {'PATH': os.pathsep.join([str(native.parent), os.environ.get('PATH', '')]), 'LANG': 'C.UTF-8',
                   'HOME': str(root / 'home'), 'XDG_CONFIG_HOME': str(root / 'xdg-config'),
                   'XDG_DATA_HOME': str(root / 'xdg-data'), 'XDG_CACHE_HOME': str(root / 'xdg-cache'),
                   'XDG_STATE_HOME': str(root / 'xdg-state'), 'OPENCODE_DISABLE_MODELS_FETCH': 'true',
                   'OPENCODE_DISABLE_AUTOUPDATE': 'true', 'OPENCODE_DISABLE_LSP': 'true',
                   'HGS_CONFIG_DIR': str(root / 'hgs-config'), 'HGS_STATE_DIR': str(cls.state),
                   'HGS_PEERS': '', 'HGS_SELF': 'fixture', 'HGS_TAB': '0', 'TMUX_TMPDIR': str(root / 'tmux')}
        found = shutil.which('opencode', path=cls.env['PATH'])
        if not found or not os.path.samefile(found, native):
            raise RuntimeError('hgs would not start the OpenCode under test: ' + str(found))
        version = cls.opencode('--version').stdout.strip()
        match = re.fullmatch(r'(?:opencode )?v?(\d+)\.(\d+)\.(\d+)', version)
        parts = tuple(int(part) for part in match.groups()) if match else None
        if not parts or parts[0] != MINIMUM[0] or parts < MINIMUM:
            raise RuntimeError('The OpenCode suite requires OpenCode %d.%d.%d or newer 2.x, got %r' % (*MINIMUM, version))
        cls.version = '%d.%d.%d' % parts
        config = {'$schema': 'https://opencode.ai/config.json', 'model': 'fixture/local', 'small_model': 'fixture/local',
                  'permission': {'*': 'ask', 'question': 'allow', 'read': 'allow'},
                  'provider': {'fixture': {'npm': '@ai-sdk/openai-compatible', 'name': 'Private keyless fixture',
                                           'options': {'baseURL': f'http://127.0.0.1:{cls.model.server_port}/v1',
                                                       'apiKey': FIXTURE_KEY},
                                           'models': {'local': {'name': 'Native fixture', 'limit': {'context': 32768, 'output': 2048}},
                                                      'alternate': {'name': 'Alternate fixture',
                                                                    'limit': {'context': 32768, 'output': 2048}},
                                                      'vendor/slash': {'name': 'Slash fixture',
                                                                       'limit': {'context': 32768, 'output': 2048}}}}}}
        (root / 'xdg-config/opencode/opencode.json').write_text(json.dumps(config))
        # A provider only the project folder configures, with the same canary key.
        folder = {'$schema': 'https://opencode.ai/config.json',
                  'provider': {'folderonly': {'npm': '@ai-sdk/openai-compatible', 'name': 'Folder-only fixture',
                                              'options': {'baseURL': f'http://127.0.0.1:{cls.model.server_port}/v1',
                                                          'apiKey': FIXTURE_KEY},
                                              'models': {'proj': {'name': 'Project model', 'limit': {'context': 32768, 'output': 2048}}}}}}
        (root / 'project/opencode.json').write_text(json.dumps(folder))
        cls.project = root / 'project'
        # A private service port before anything can start the service.
        cls.port = free_port()
        while cls.port == DEFAULT_PORT:
            cls.port = free_port()
        cls.opencode('service', 'set', 'port', str(cls.port))
        configured = json.loads((root / 'xdg-config/opencode/service.json').read_text())
        if configured.get('port') != cls.port:
            raise RuntimeError('OpenCode did not record the private service port')

    @classmethod
    def opencode(cls, *args):
        """An OpenCode command, always under the private environment."""
        run = subprocess.run([NATIVE, *args], env=cls.env, cwd=cls.workspace, stdin=subprocess.DEVNULL,
                             capture_output=True, text=True, timeout=60)
        if run.returncode:
            raise RuntimeError('opencode %s failed: %s' % (' '.join(args), (run.stderr or run.stdout)[-800:]))
        return run

    @classmethod
    def registration_path(cls):
        return cls.root / 'xdg-state/opencode/service.json'

    @classmethod
    def native_processes(cls):
        return [process for process in owned_processes(str(cls.root)) if 'opencode' in process[1]]

    @classmethod
    def stop_service(cls):
        """Stop the service OpenCode registered privately and wait until no process
        carries the private root. A registration this suite wrote itself (a stub, a hung
        port or an exited process) is deleted, never handed to `opencode service stop`."""
        registration = cls.registration_path()
        try:
            written = registration.read_bytes()
        except OSError:
            written = None
        if written in cls.fake_registrations:
            registration.unlink()
        elif written is not None:
            cls.opencode('service', 'stop')
        until = time.monotonic() + 15
        while owned_processes(str(cls.root)) and time.monotonic() < until:
            time.sleep(.2)

    @classmethod
    def reap(cls):
        """Stop whatever still carries the private root, only this suite's processes;
        returns what was found."""
        leftovers = owned_processes(str(cls.root))
        for sig in (signal.SIGTERM, signal.SIGKILL) if leftovers else ():
            for pid, _ in owned_processes(str(cls.root)):
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass
            time.sleep(1)
        return leftovers

    @classmethod
    def cleanup(cls):
        try:
            if cls.model:
                cls.model.release.set()
            if hasattr(cls, 'env'):
                cls.stop_service()
        finally:
            leftovers = cls.reap()
            if cls.model:
                cls.model.shutdown()
                cls.model.server_close()
            cls.temp.cleanup()
        if leftovers:
            raise RuntimeError('Processes outlived the private OpenCode service: ' + json.dumps(leftovers))

    @classmethod
    def tearDownClass(cls):
        cls.cleanup()

    def setUp(self):
        self.model.holding.clear()
        self.model.release.clear()
        self.model.requests.clear()
        self.addCleanup(self.stop_and_check)
        self.addCleanup(self.model.release.set)

    def stop_and_check(self):
        """Each test ends with its private service stopped and nothing left behind, so a
        leak is reported by the test that caused it."""
        self.stop_service()
        self.assertEqual(self.reap(), [], 'processes outlived this test')

    def hgs(self, args, payload=None, success=True):
        run = subprocess.run([str(HGS), *args], env=self.env, cwd=self.workspace, text=True, capture_output=True,
                             input=None if payload is None else json.dumps(payload), timeout=60)
        if success:
            self.assertEqual(run.returncode, 0, run.stderr)
        else:
            self.assertNotEqual(run.returncode, 0, run.stdout)
        return json.loads(run.stdout) if run.returncode == 0 and run.stdout.strip().startswith(('{', '[')) else run

    def launch(self, tag, *options, success=True):
        return self.hgs(['opencode', str(self.workspace), '--new', '-n', tag, '-d', '--launch-id', str(uuid.uuid4()),
                         '--account', 'native-opencode', *options], success=success)

    def create(self, *options):
        tag = uuid.uuid4().hex[:10]
        self.launch(tag, '--model', 'fixture/local', *options)
        return self.hgs(['inspect', f'opencode/workspace/{tag}'])

    def payload(self, binding, **extra):
        return {'request_id': str(uuid.uuid4()), 'expected_run_id': binding['run_id'],
                'expected_conversation_id': binding['conversation_id'], **extra}

    def action(self, binding, action, status='completed', **extra):
        """A scoped session action; its receipt, not the exit status, reports a refusal."""
        receipt = self.hgs(['session-action', binding['name'], '--json'], {**self.payload(binding, action=action), **extra})
        self.assertEqual(receipt['status'], status, receipt)
        return receipt

    def wait(self, name, predicate, timeout=40):
        until = time.monotonic() + timeout
        while True:
            value = self.hgs(['inspect', name])
            if predicate(value):
                return value
            if time.monotonic() > until:
                self.fail('Native state did not settle: ' + json.dumps(value)[-3000:])
            time.sleep(.25)

    def binding_path(self, name):
        return self.state / 'opencode/bindings' / (hashlib.sha256(name.encode()).hexdigest() + '.json')

    def save_binding(self, tag, **fields):
        """A binding written as an earlier hgs would have left it."""
        name = 'opencode/workspace/' + tag
        record = {'backend': 'opencode', 'agent': 'opencode', 'name': name, 'cwd': os.path.realpath(self.workspace),
                  'run_id': str(uuid.uuid4()), 'conversation_id': 'ses_' + native_suffix(), 'created': time.time(),
                  'launch_id': None, 'agent_home': str(self.root / 'xdg-config/opencode'), **fields}
        path = self.binding_path(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(record))
        return record

    def receipt_path(self, request):
        return self.state / 'opencode/receipts' / (request['request_id'] + '.json')

    def registration(self):
        return json.loads(self.registration_path().read_text())

    def write_registration(self, value):
        path = self.registration_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        written = json.dumps(value).encode()
        self.fake_registrations.add(written)
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, 'wb') as file:
            file.write(written)
        return written

    def api(self, method, path, body=None):
        """This test's own request to its private service, authenticated as hgs is."""
        registration = self.registration()
        headers = {'Content-Type': 'application/json'}
        if registration.get('password'):
            token = base64.b64encode(('opencode:' + registration['password']).encode()).decode()
            headers['Authorization'] = 'Basic ' + token
        request = urllib.request.Request(registration['url'].rstrip('/') + path, method=method, headers=headers,
                                         data=None if body is None else json.dumps(body).encode())
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            with error:
                raw = error.read()
            return error.code, json.loads(raw) if raw else None

    def session(self, conversation_id):
        status, answer = self.api('GET', '/api/session/' + conversation_id)
        self.assertEqual(status, 200, answer)
        return answer['data']

    def user_messages(self, conversation_id):
        status, page = self.api('GET', f'/api/session/{conversation_id}/message?limit=100')
        self.assertEqual(status, 200, page)
        return [(m['id'], m.get('text')) for m in page['data'] if m.get('type') == 'user']

    def inbox(self, conversation_id):
        status, answer = self.api('GET', f'/api/session/{conversation_id}/inbox')
        self.assertEqual(status, 200, answer)
        return answer['data']

    def listing(self):
        rows = self.hgs(['ls', '--local', '--json', '--no-archives'])['sessions']
        return {row['name']: row for row in rows if row.get('backend') == 'opencode'}

    def account(self):
        profiles = self.hgs(['account', 'ls'])['profiles']
        return next(profile for profile in profiles if profile['id'] == 'native-opencode')

    def ready_account(self):
        """Start the private service explicitly and wait until its catalog is ready."""
        self.stop_service()
        self.hgs(['account', 'start', 'native-opencode'])
        until = time.monotonic() + 30
        account = self.account()
        while account['catalog_state'] == 'loading' and time.monotonic() < until:
            time.sleep(.5)
            account = self.account()
        self.assertEqual(account['catalog_state'], 'ready', account)
        return account

    def catalog(self, location=None, success=True):
        """`hgs account models` for the home folder, or for `location`."""
        where = [] if location is None else ['--location', str(location)]
        return self.hgs(['account', 'models', 'native-opencode', *where, '--json'], success=success)

    def favorite(self, model, on, request=None, success=True):
        return self.hgs(['account', 'favorite', 'native-opencode', '--model', model, '--favorite', 'on' if on else 'off',
                         '--request-id', request or str(uuid.uuid4()), '--json'], success=success)

    def opencode_files(self, *bases):
        """sha256 of every file under the private folders `bases`, by path below the private root."""
        found = {}
        for base in bases:
            for path in sorted((self.root / base).rglob('*')):
                if path.is_file() and not path.is_symlink():
                    found[str(path.relative_to(self.root))] = hashlib.sha256(path.read_bytes()).hexdigest()
        return found

    def assert_folder_free(self, answer):
        """Neither the folder-only provider nor its model is in `answer`."""
        self.assertNotIn('folderonly', [p['id'] for p in answer['providers']], answer['providers'])
        values = [m['value'] for m in answer['model_options']] + [m['value'] for m in answer.get('models', [])]
        self.assertNotIn('folderonly/proj', values)

    def assert_private(self, *values):
        text = json.dumps(values)
        self.assertNotIn(FIXTURE_KEY, text)
        if self.registration_path().exists():
            registration = self.registration()
            if registration.get('password'):
                self.assertNotIn(registration['password'], text)

    def test_01_not_running_reports_unknown_and_never_starts(self):
        self.stop_service()
        self.assertFalse(self.registration_path().exists())
        binding = self.save_binding('saved-' + uuid.uuid4().hex[:8], service='opencode2')
        name = binding['name']
        row = self.hgs(['inspect', name])
        self.assertTrue(row['inspection_unavailable'])
        # The computer is named as hgs names itself (HGS_SELF), like the desktop does.
        self.assertEqual(row['inspection_reason'], NOT_RUNNING + ' on fixture')
        # Native state stays unknown, never stopped; clients keep the transcript they show.
        self.assertEqual((row['state'], row['phase'], row['activity']), ('unknown', 'unknown', 'unknown'))
        self.assertEqual(row['allowed_actions'], ['forget'])
        self.assertNotIn('events', row)
        self.assertFalse(row['send_supported'])
        listed = self.listing()[name]
        self.assertEqual((listed['state'], listed['phase']), ('unknown', 'unknown'))
        self.assertEqual(listed['inspection_reason'], NOT_RUNNING + ' on fixture')
        account = self.account()
        self.assertEqual(account['catalog_state'], 'not_running')
        self.assertEqual(account['catalog_reason'], NOT_RUNNING + ' on fixture')
        # Sending is explicit, but it never starts the service either.
        request = self.payload(binding, text='never sent')
        failure = self.hgs(['send', name, '--json'], request, success=False)
        self.assertIn(NOT_RUNNING + ' on fixture', failure.stderr)
        self.assertFalse(self.receipt_path(request).exists())
        self.assertFalse(self.registration_path().exists())
        self.assertEqual(self.native_processes(), [])

    def test_native_terminal_attaches_without_starting_or_stopping_the_service(self):
        import fcntl
        import pty
        import select
        import struct
        import termios
        binding = self.create()
        name, run = binding['name'], binding['run_id']
        registration = self.registration()
        refused = self.hgs(['a', name, '--existing', '--run-id', str(uuid.uuid4())], success=False)
        self.assertIn('changed', refused.stderr.lower())
        master, slave = pty.openpty()
        child = None
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
            environment = {**self.env, 'TERM': 'xterm-256color', 'COLORTERM': 'truecolor', 'HGS_TAB': '0'}
            child = subprocess.Popen([str(HGS), 'a', name, '--existing', '--run-id', run], env=environment,
                                     cwd=self.workspace, stdin=slave, stdout=slave, stderr=slave,
                                     start_new_session=True)
            os.close(slave); slave = -1
            output = bytearray(); end = time.monotonic() + 25
            title = self.session(binding['conversation_id'])['title']
            text = ''
            while time.monotonic() < end and child.poll() is None:
                if select.select([master], [], [], min(0.2, max(0, end - time.monotonic())))[0]:
                    try:
                        chunk = os.read(master, min(8192, 262144 - len(output)))
                    except OSError:
                        break
                    if not chunk:
                        break
                    output.extend(chunk)
                    self.assertLess(len(output), 262144, 'native TUI output exceeded the fixture cap')
                    text = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', output.decode('utf-8', errors='replace'))
                    if title in text:
                        break
            self.assertIsNone(child.poll(), text[-2000:])
            self.assertIn(title, text, 'native TUI did not render the existing conversation')
            self.assertEqual(self.registration(), registration)
            inspected = self.hgs(['inspect', name])
            self.assertEqual((inspected['run_id'], inspected['conversation_id']),
                             (run, binding['conversation_id']))
            self.assertEqual(len(self.api('GET', '/api/session/' + binding['conversation_id'] + '/message')[1]['data']), 0)
            if registration.get('password'):
                self.assertNotIn(registration['password'], text)
        finally:
            if slave >= 0:
                os.close(slave)
            if child is not None:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        child.kill(); child.wait(timeout=5)
                else:
                    child.wait()
            os.close(master)
        self.assertEqual(self.registration(), registration)
        self.assertEqual(self.session(binding['conversation_id'])['id'], binding['conversation_id'])

    def test_leading_absolute_path_is_literal_message_text(self):
        binding = self.create()
        text = '/help/fixture/capture.png\nWhat is shown in this screenshot?'
        request = self.payload(binding, text=text)
        receipt = self.hgs(['send', binding['name'], '--json'], request)
        self.assertEqual(receipt['status'], 'submitted')
        done = self.wait(binding['name'], lambda v: v['phase'] == 'idle' and v.get('reply_id'))
        self.assertEqual([e['detail'] for e in done['events'] if e['type'] == 'UserPromptSubmit'], [text])
        self.assertEqual([message for _, message in self.user_messages(binding['conversation_id'])], [text])
        self.assertEqual(self.hgs(['send', binding['name'], '--json'], request), receipt)
        self.assertEqual([message for _, message in self.user_messages(binding['conversation_id'])], [text])

    def test_02_launch_starts_service_with_own_ids_and_history(self):
        self.stop_service()
        # A registration left by a service that is gone does not block the start.
        gone = subprocess.Popen(['true'])
        gone.wait()
        self.write_registration({'id': 'svc_gone', 'version': self.version, 'url': f'http://127.0.0.1:{self.port}',
                                 'pid': gone.pid, 'password': 'gone-password'})
        tag, launch_id = uuid.uuid4().hex[:10], str(uuid.uuid4())
        self.hgs(['opencode', str(self.workspace), '--new', '-n', tag, '-d', '--launch-id', launch_id,
                  '--account', 'native-opencode', '--model', 'fixture/local'])
        registration = self.registration()
        self.assertNotEqual(registration['pid'], gone.pid)
        os.kill(registration['pid'], 0)
        self.assertEqual(registration['url'].rstrip('/'), f'http://127.0.0.1:{self.port}')
        self.assertTrue(self.native_processes())
        name = 'opencode/workspace/' + tag
        b = self.hgs(['inspect', name])
        self.assertRegex(b['conversation_id'], r'^ses_[A-Za-z0-9]{26}$')
        self.assertEqual(b['launch_id'], launch_id)
        self.assertFalse(b['terminal_supported'])
        self.assertTrue(b['terminal_attach_supported'])
        self.assertFalse(b['native_ui_available'])
        self.assertEqual((b['state'], b['phase'], b['settings_model']), ('running', 'idle', 'fixture/local'))
        session = self.session(b['conversation_id'])
        self.assertEqual(session['metadata']['zerus'], {'run_id': b['run_id'], 'launch_id': launch_id})
        self.assertEqual(session['location']['directory'], os.path.realpath(self.workspace))
        self.assertEqual(session['location']['directory'], b['cwd'])
        self.assertEqual((session['model']['providerID'], session['model']['id'], session['title']), ('fixture', 'local', tag))
        request = self.payload(b, text='hello')
        first = self.hgs(['send', name, '--json'], request)
        self.assertEqual(first['status'], 'submitted')
        self.assertRegex(first['native_message_id'], r'^msg_[A-Za-z0-9]{26}$')
        # The same request ID returns the same receipt; other text under it is refused.
        self.assertEqual(self.hgs(['send', name, '--json'], request), first)
        self.hgs(['send', name, '--json'], {**request, 'text': 'changed'}, success=False)
        for field in ['expected_run_id', 'expected_conversation_id']:
            self.hgs(['send', name, '--json'], {**self.payload(b, text='wrong target'), field: 'wrong'}, success=False)
        done = self.wait(name, lambda v: v['phase'] == 'idle' and v.get('reply_id'))
        events = [(e['type'], e['detail']) for e in done['events']]
        self.assertEqual([detail for kind, detail in events if kind == 'UserPromptSubmit'], ['hello'])
        prompt = events.index(('UserPromptSubmit', 'hello'))
        reply = events.index(('AgentMessage', 'Native OpenCode reply'))
        stop = next(i for i, (kind, _) in enumerate(events) if kind == 'Stop')
        self.assertLess(prompt, reply)
        self.assertLess(reply, stop)
        page = self.hgs(['history', name, '--json'], self.payload(b, limit=100))
        self.assertTrue(page['head']['complete'])
        self.assertTrue(any(e.get('detail') == 'Native OpenCode reply' for e in page['events']), page)
        self.assert_private(b, first, done, page)
        # The response to an admitted message is lost: the same request finds it, once.
        receipt = json.loads(self.receipt_path(request).read_text())
        receipt['status'] = 'pending'
        self.receipt_path(request).write_text(json.dumps(receipt))
        self.assertEqual(self.hgs(['send', name, '--json'], request)['status'], 'submitted')
        self.assertEqual(self.user_messages(b['conversation_id']), [(first['native_message_id'], 'hello')])
        # The response to a launch is lost: the journaled session ID is created once.
        lost_tag, lost_id = uuid.uuid4().hex[:10], 'ses_' + native_suffix()
        run_id, cwd = str(uuid.uuid4()), os.path.realpath(self.workspace)
        body = {'id': lost_id, 'location': {'directory': cwd}, 'title': lost_tag,
                'metadata': {'zerus': {'run_id': run_id, 'launch_id': None}}, 'model': {'providerID': 'fixture', 'id': 'local'}}
        self.save_binding(lost_tag, service='opencode2', run_id=run_id, conversation_id=lost_id, creation_pending=True,
                          creation_body=body)
        self.assertEqual(self.api('GET', '/api/session/' + lost_id)[0], 404)
        for _ in range(2):
            self.hgs(['opencode', str(self.workspace), '-n', lost_tag, '-d', '--account', 'native-opencode'])
        created = self.session(lost_id)
        self.assertEqual((created['title'], created['metadata']['zerus']['run_id']), (lost_tag, run_id))
        saved = json.loads(self.binding_path('opencode/workspace/' + lost_tag).read_text())
        self.assertNotIn('creation_pending', saved)
        status, sessions = self.api('GET', '/api/session?limit=100')
        self.assertEqual(status, 200, sessions)
        self.assertEqual([s['id'] for s in sessions['data'] if s.get('title') == lost_tag], [lost_id])
        self.assertEqual(self.hgs(['inspect', 'opencode/workspace/' + lost_tag])['phase'], 'idle')

    def external_conversation(self):
        """Create a fixture through the native API, not through a Zerus launch."""
        self.ready_account()
        conversation = 'ses_' + native_suffix()
        body = {'id': conversation, 'location': {'directory': os.path.realpath(self.workspace)},
                'title': 'Existing native title', 'metadata': {'owner_fixture': 'keep-this'},
                'model': {'providerID': 'fixture', 'id': 'local'}}
        status, answer = self.api('POST', '/api/session', body)
        self.assertIn(status, (200, 201), answer)
        return conversation

    def binding_request(self, conversation, **extra):
        return {'schema_version': 1, 'request_id': str(uuid.uuid4()),
                'name': 'opencode/workspace/adopt-' + uuid.uuid4().hex[:8],
                'conversation_id': conversation, 'directory': os.path.realpath(self.workspace),
                'agent_home': str(self.root / 'xdg-config/opencode'), 'preview': True, **extra}

    def bind_existing(self, request, success=True):
        return self.hgs(['account', 'bind-existing', 'native-opencode', '--json'], request, success=success)

    def test_30_existing_preview_bind_reopen_preserves_native_history(self):
        conversation = self.external_conversation()
        message = 'msg_' + native_suffix()
        status, reply = self.api('POST', f'/api/session/{conversation}/prompt',
                                 {'id': message, 'text': 'Before Zerus binding', 'delivery': 'queue'})
        self.assertIn(status, (200, 202), reply)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            _, page = self.api('GET', f'/api/session/{conversation}/message?limit=100')
            if (any(m.get('type') == 'assistant' and m.get('time', {}).get('completed')
                    and any(c.get('text') == 'Native OpenCode reply' for c in m.get('content', []))
                    for m in page['data']) and any(m.get('type') == 'idle' for m in page['data'])):
                break
            time.sleep(.1)
        else:
            self.fail('The native fixture did not finish its pre-binding reply')
        native_before = self.session(conversation)
        messages_before = self.api('GET', f'/api/session/{conversation}/message?limit=100')[1]
        registration = self.registration()
        request = self.binding_request(conversation)
        preview = self.bind_existing(request)
        self.assertEqual(preview['status'], 'preview')
        self.assertFalse(self.binding_path(request['name']).exists())
        request.update(preview=False, expected_identity=preview['identity'])
        bound = self.bind_existing(request)
        self.assertEqual(bound['status'], 'bound')
        self.assertEqual(self.session(conversation), native_before)
        self.assertEqual(self.api('GET', f'/api/session/{conversation}/message?limit=100')[1], messages_before)
        self.assertEqual(self.registration(), registration)
        replay = self.bind_existing(request)
        self.assertEqual((replay['status'], replay['run_id']), ('already_bound', bound['run_id']))
        detail = self.hgs(['inspect', request['name']])
        self.assertEqual((detail['conversation_id'], detail['run_id']), (conversation, bound['run_id']))
        page = self.hgs(['history', request['name'], '--json'], self.payload(detail, limit=100))
        self.assertTrue(any(e.get('detail') == 'Before Zerus binding' for e in page['events']), page)
        submit = self.payload(detail, text='Continue the existing conversation')
        once = self.hgs(['send', request['name'], '--json'], submit)
        self.assertEqual(self.hgs(['send', request['name'], '--json'], submit), once)
        self.wait(request['name'], lambda v: v['phase'] == 'idle' and v.get('reply_id') != detail.get('reply_id'))
        self.assertCountEqual([text for _, text in self.user_messages(conversation)],
                              ['Before Zerus binding', 'Continue the existing conversation'])
        self.assertEqual(self.session(conversation)['metadata'], native_before['metadata'])
        self.assertEqual(self.session(conversation)['title'], native_before['title'])
        self.assertEqual(self.hgs(['inspect', request['name']])['run_id'], bound['run_id'])

    def test_31_existing_binding_refuses_duplicates_profile_and_directory_changes(self):
        conversation = self.external_conversation()
        request = self.binding_request(conversation)
        for fields in [{'agent_home': str(self.root / 'other-profile')}, {'directory': str(self.project)},
                       {'conversation_id': 'ses_missing'}, {'name': 'opencode/../unsafe'}, {'extra': True}]:
            self.bind_existing({**request, **fields}, success=False)
        self.bind_existing({**request, 'preview': False}, success=False)
        preview = self.bind_existing(request)
        commit = {**request, 'preview': False, 'expected_identity': preview['identity']}
        self.bind_existing(commit)
        self.bind_existing({**request, 'name': request['name'] + '-duplicate', 'request_id': str(uuid.uuid4())}, success=False)
        conflict = self.binding_request('ses_' + native_suffix(), name=request['name'])
        self.bind_existing(conflict, success=False)
        # Forget only removes the Zerus owner. Replaying an old bind cannot undo it.
        detail = self.hgs(['inspect', request['name']])
        self.action(detail, 'forget')
        self.bind_existing(commit, success=False)
        self.assertFalse(self.binding_path(request['name']).exists())
        self.assertEqual(self.session(conversation)['id'], conversation)

    def test_32_existing_binding_never_starts_a_service(self):
        self.stop_service()
        request = self.binding_request('ses_' + native_suffix())
        before = self.opencode_files('xdg-state', 'xdg-config/opencode')
        self.bind_existing(request, success=False)
        self.assertEqual(self.opencode_files('xdg-state', 'xdg-config/opencode'), before)
        self.assertFalse(self.binding_path(request['name']).exists())
        self.assertFalse(self.native_processes())

    def test_33_existing_binding_preview_is_invalidated_by_a_service_restart(self):
        conversation = self.external_conversation()
        request = self.binding_request(conversation)
        preview = self.bind_existing(request)
        self.stop_service()
        self.ready_account()
        self.bind_existing({**request, 'preview': False, 'expected_identity': preview['identity']}, success=False)
        self.assertFalse(self.binding_path(request['name']).exists())
        refreshed = self.bind_existing(request)
        self.assertNotEqual(refreshed['identity'], preview['identity'])

    def test_03_lost_response_and_queue_never_duplicate(self):
        b = self.create()
        name, conversation = b['name'], b['conversation_id']
        self.hgs(['send', name, '--json'], self.payload(b, text='HOLD_NATIVE first turn'))
        self.assertTrue(self.model.holding.wait(30), 'the model never received the held turn')
        working = self.wait(name, lambda v: v['phase'] == 'working' and v['interrupt_supported'])
        request = self.payload(b, text='Queued while working')
        queued = self.hgs(['send', name, '--json'], request)
        self.assertEqual(queued['status'], 'submitted')
        self.assertTrue(queued.get('queued'))
        message_id = queued['native_message_id']
        # The admission's response is lost: the journal still says pending.
        receipt = json.loads(self.receipt_path(request).read_text())
        receipt['status'] = 'pending'
        self.receipt_path(request).write_text(json.dumps(receipt))
        again = self.hgs(['send', name, '--json'], request)
        self.assertEqual((again['status'], again['native_message_id']), ('submitted', message_id))
        self.assertEqual([item['id'] for item in self.inbox(conversation)], [message_id])
        view = self.wait(name, lambda v: any(e.get('delivery_state') == 'queued' for e in v['events']))
        rows = [e for e in view['events'] if e['detail'] == 'Queued while working']
        self.assertEqual([(e['message_id'], e['delivery_state']) for e in rows], [(message_id, 'queued')])
        self.assertNotIn('remove_queued', view['allowed_actions'])
        # Stop ends the turn; the queued message stays, not delivered.
        stop = {'request_id': str(uuid.uuid4()), 'expected_run_id': b['run_id'], 'expected_conversation_id': conversation,
                'expected_turn_started': working['turn_started']}
        self.hgs(['interrupt', name, '--json'], stop)
        self.model.release.set()
        stopped = self.wait(name, lambda v: v['phase'] == 'idle'
                            and any(e.get('delivery_state') == 'not_delivered' for e in v['events']))
        self.assertIn('remove_queued', stopped['allowed_actions'])
        self.assertEqual([item['id'] for item in self.inbox(conversation)], [message_id])
        self.action(b, 'remove_queued', message_id=message_id)
        self.assertEqual(self.inbox(conversation), [])
        cleared = self.wait(name, lambda v: not any(e.get('delivery_state') for e in v['events']))
        self.assertNotIn('remove_queued', cleared['allowed_actions'])
        refused = self.action(b, 'remove_queued', status='failed', message_id=message_id)
        self.assertEqual(refused['error_source'], 'opencode')
        self.assertEqual(refused['error'],
                         'OpenCode could not complete this action; inspect the session on its computer')
        self.assert_private(refused)
        # A confirmed removal is the request's definite non-delivery.
        removed = json.loads(self.receipt_path(request).read_text())
        self.assertEqual((removed['status'], removed['reason']), ('refused', REMOVED))
        self.assertIn(REMOVED, self.hgs(['send', name, '--json'], request, success=False).stderr)
        # A journaled message whose request never reached OpenCode is never posted by a
        # retry: OpenCode would admit an unknown ID, so its delivery stays uncertain.
        lost = self.payload(b, text='Never posted after a lost request')
        lost_id = 'msg_' + native_suffix()
        digest = hashlib.sha256(json.dumps([name, lost], sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        self.receipt_path(lost).write_text(json.dumps({
            'request_id': lost['request_id'], 'name': name, 'run_id': b['run_id'], 'conversation_id': conversation,
            'digest': digest, 'native_message_id': lost_id, 'text': lost['text'], 'submitted_text': lost['text'],
            'submitted_at': time.time(), 'at': time.time(), 'status': 'pending', 'attachments': [],
            'transport': 'native_opencode'}))
        failure = self.hgs(['send', name, '--json'], lost, success=False)
        self.assertIn('Delivery uncertain; this request will not be replayed', failure.stderr)
        self.assertEqual(json.loads(self.receipt_path(lost).read_text())['status'], 'pending')
        self.assertEqual(self.inbox(conversation), [])
        users = self.user_messages(conversation)
        self.assertEqual([text for _, text in users], ['HOLD_NATIVE first turn'])
        self.assertNotIn(message_id, [native_id for native_id, _ in users])
        self.assertEqual(self.hgs(['inspect', name])['phase'], 'idle')

    def test_04_question_and_permission_answers(self):
        b = self.create()
        name = b['name']
        self.hgs(['send', name, '--json'], self.payload(b, text='QUESTION_NATIVE pick a color'))
        data = self.wait(name, lambda v: v['pending_questions'])
        self.assertEqual((data['phase'], data['activity']), ('input', 'attention'))
        self.assertTrue(data.get('attention_id'))
        [card] = data['pending_questions']
        self.assertEqual((card['kind'], card['can_answer'], card['approval']), ('opencode_form', True, False))
        [item] = card['questions']
        self.assertEqual([o['label'] for o in item['options']], ['Green', 'Blue'])
        green = next(o['id'] for o in item['options'] if o['label'] == 'Green')
        # A pending question blocks new messages instead of queueing them behind it.
        self.hgs(['send', name, '--json'], self.payload(b, text='not while asked'), success=False)
        request = self.payload(b, question_id=card['question_id'], expected_question_hash=card['question_hash'],
                               answers=[{'question_id': item['id'], 'selected_option_ids': [green], 'text': ''}])
        self.hgs(['answer', name, '--json'], {**request, 'expected_question_hash': 'changed'}, success=False)
        self.assertEqual(self.hgs(['answer', name, '--json'], request)['status'], 'answered')
        self.hgs(['answer', name, '--json'], request, success=False)
        final = self.wait(name, lambda v: v['phase'] == 'idle' and v.get('reply_id'))
        self.assertTrue(any(e['type'] == 'PreToolUse' and e['tool'] == 'question' for e in final['events']))
        self.assertTrue(any(e['type'] == 'PostToolUse' and e['tool'] == 'question' and '"Green"' in e['detail']
                            for e in final['events']), final['events'])
        self.assertEqual(final['pending_questions'], [])
        tool = self.create()
        self.hgs(['send', tool['name'], '--json'], self.payload(tool, text='BASH_NATIVE run the fixture command'))
        data = self.wait(tool['name'], lambda v: v['pending_questions'])
        [card] = data['pending_questions']
        self.assertEqual((card['kind'], card['can_answer'], card['approval']), ('opencode_permission', True, True))
        [item] = card['questions']
        self.assertEqual([o['id'] for o in item['options']], ['once', 'always', 'reject'])
        self.assertIn('printf native-tool-output', item['prompt'])
        request = self.payload(tool, question_id=card['question_id'], expected_question_hash=card['question_hash'],
                               answers=[{'question_id': 'permission', 'selected_option_ids': ['once'], 'text': ''}])
        self.assertEqual(self.hgs(['answer', tool['name'], '--json'], request)['status'], 'answered')
        final = self.wait(tool['name'], lambda v: v['phase'] == 'idle' and v.get('reply_id'))
        self.assertTrue(any(e['type'] == 'PostToolUse' and e['tool'] == 'shell' and 'native-tool-output' in e['detail']
                            for e in final['events']), final['events'])
        self.assert_private(data, final)

    def test_05_model_effort_and_provider_error(self):
        b = self.create('--effort', 'low')
        name = b['name']
        self.assertEqual((b['settings_model'], b['effort']), ('fixture/local', 'low'))
        self.assertEqual(self.session(b['conversation_id'])['model'], {'providerID': 'fixture', 'id': 'local', 'variant': 'low'})
        options = {row['value']: row for row in b['model_options']}
        self.assertEqual(set(options['fixture/alternate']['effort_options']), {'low', 'medium', 'high'})
        self.hgs(['settings', name, '--json'], self.payload(b, model='fixture/alternate', effort='extreme'), success=False)
        self.hgs(['settings', name, '--json'], self.payload(b, model='fixture/missing', effort=''), success=False)
        applied = self.hgs(['settings', name, '--json'], self.payload(b, model='fixture/alternate', effort='high'))
        self.assertEqual((applied['status'], applied['model'], applied['effort']), ('applied', 'fixture/alternate', 'high'))
        self.assertEqual(self.session(b['conversation_id'])['model'],
                         {'providerID': 'fixture', 'id': 'alternate', 'variant': 'high'})
        changed = self.hgs(['inspect', name])
        self.assertEqual((changed['settings_model'], changed['effort']), ('fixture/alternate', 'high'))
        self.hgs(['send', name, '--json'], self.payload(b, text='ERROR_NATIVE fail'))
        error = self.wait(name, lambda v: v['phase'] == 'error')
        self.assertEqual(error['activity'], 'attention')
        self.assertTrue(any(e['type'] == 'StopFailure' and 'Fixture provider rejected' in e['detail'] for e in error['events']))
        self.assertIn('Fixture provider rejected', json.dumps(error))
        # The failed turn went to the changed model with the changed effort.
        turns = [r for r in self.model.requests if 'ERROR_NATIVE' in r['last_user'] and r['tools']]
        self.assertTrue(turns)
        self.assertEqual({(r['model'], r['options'].get('reasoning_effort')) for r in turns}, {('alternate', 'high')})
        self.assert_private(b, changed, error)

    def test_06_legacy_binding(self):
        current = self.create()
        cwd = os.path.realpath(self.workspace)
        refused = self.save_binding('legacy-' + uuid.uuid4().hex[:8], conversation_id='ses_' + uuid.uuid4().hex,
                                    paused=True, native_data_home=str(self.root / 'opencode1-data'))
        # A session OpenCode 2 migrated keeps its OpenCode 1 ID and directory.
        migrated_id = 'ses_' + uuid.uuid4().hex
        status, created = self.api('POST', '/api/session', {'id': migrated_id, 'location': {'directory': cwd}, 'title': 'migrated'})
        self.assertEqual(status, 200, created)
        migrated = self.save_binding('migrated-' + uuid.uuid4().hex[:8], conversation_id=migrated_id, paused=True)
        rows = self.listing()
        legacy = rows[refused['name']]
        self.assertEqual((legacy['state'], legacy['allowed_actions']), ('stopped', ['forget']))
        self.assertEqual(legacy['inspection_reason'], OPENCODE1)
        self.assertEqual((rows[current['name']]['state'], rows[current['name']]['phase']), ('running', 'idle'))
        self.assertIn('rename', rows[current['name']]['allowed_actions'])
        self.assertEqual((rows[migrated['name']]['state'], rows[migrated['name']]['phase']), ('running', 'idle'))
        adopted = json.loads(self.binding_path(migrated['name']).read_text())
        self.assertEqual(adopted['service'], 'opencode2')
        self.assertNotIn('paused', adopted)
        inspected = self.hgs(['inspect', refused['name']])
        self.assertEqual((inspected['state'], inspected['allowed_actions']), ('stopped', ['forget']))
        self.assertEqual(inspected['inspection_reason'], OPENCODE1)
        full = self.hgs(['inspect', current['name']])
        self.assertEqual(full['phase'], 'idle')
        self.assertIn('fixture/local', [row['value'] for row in full['model_options']])
        self.hgs(['send', refused['name'], '--json'], self.payload(refused, text='never delivered'), success=False)
        # Forget removes only the Zerus binding; OpenCode's sessions stay.
        for record in (refused, migrated):
            self.action(record, 'forget')
            self.assertFalse(self.binding_path(record['name']).exists())
        self.assertTrue(self.binding_path(current['name']).exists())
        self.assertEqual(self.session(migrated_id)['id'], migrated_id)
        self.assertEqual(self.session(current['conversation_id'])['id'], current['conversation_id'])
        self.assertEqual(set(self.listing()) & {refused['name'], migrated['name']}, set())
        self.assertEqual(self.hgs(['inspect', current['name']])['phase'], 'idle')

    def live_stand_in(self):
        """A disposable live process to register as the service: if hgs ever ran OpenCode's
        own start here, OpenCode could stop that process, never this test runner."""
        child = subprocess.Popen(['sleep', '600'], stdin=subprocess.DEVNULL)
        self.addCleanup(child.wait)
        self.addCleanup(child.kill)
        return child

    def assert_never_started(self, registration, stand_in):
        self.assertEqual(self.registration_path().read_bytes(), registration)
        self.assertIsNone(stand_in.poll(), 'the registered process was stopped')
        self.assertEqual(self.native_processes(), [])

    def test_07_incompatible_service(self):
        self.stop_service()
        saved = self.save_binding('incompatible-' + uuid.uuid4().hex[:8], service='opencode2')
        openapi = json.loads((REPO / 'tests/fixtures/opencode2/openapi-2.0.32.json').read_text())
        del openapi['paths']['/api/session/{sessionID}/inbox']
        stand_in = self.live_stand_in()
        for version, reason in [('2.0.31', 'OpenCode 2.0.31 is not supported; install OpenCode 2.0.32 or newer 2.x'),
                                ('2.0.40', 'OpenCode 2.0.40 lacks GET /api/session/{sessionID}/inbox; update OpenCode or Zerus')]:
            with self.subTest(version=version):
                server, seen = stub_service(version, stand_in.pid, openapi)
                try:
                    registration = self.write_registration({
                        'id': 'svc_stub_' + uuid.uuid4().hex[:8], 'version': version, 'pid': stand_in.pid,
                        'url': f'http://127.0.0.1:{server.server_port}', 'password': 'stub-password'})
                    row = self.hgs(['inspect', saved['name']])
                    self.assertTrue(row['inspection_unavailable'])
                    self.assertEqual((row['inspection_reason'], row['state']), (reason, 'unknown'))
                    account = self.account()
                    self.assertEqual((account['catalog_state'], account['catalog_reason']), ('incompatible', reason))
                    tag = 'refused-' + uuid.uuid4().hex[:8]
                    self.assertIn(reason, self.launch(tag, '--model', 'fixture/local', success=False).stderr)
                    self.assertFalse(self.binding_path('opencode/workspace/' + tag).exists())
                    self.assertIn(reason, self.hgs(['account', 'start', 'native-opencode'], success=False).stderr)
                    self.assert_never_started(registration, stand_in)
                    self.assertEqual({method for method, _ in seen}, {'GET'})
                    self.assertTrue({path for _, path in seen} <= {'/api/info', '/openapi.json'}, seen)
                finally:
                    server.shutdown()
                    server.server_close()

    def test_08_never_replaces_a_live_registration(self):
        self.stop_service()
        # A live registered process whose port accepts connections but never answers.
        stand_in = self.live_stand_in()
        with socket.socket() as hung:
            hung.bind(('127.0.0.1', 0))
            hung.listen(16)
            registration = self.write_registration({
                'id': 'svc_hung', 'version': self.version, 'pid': stand_in.pid,
                'url': f'http://127.0.0.1:{hung.getsockname()[1]}', 'password': 'hung-password'})
            tag = 'hung-' + uuid.uuid4().hex[:8]
            failure = self.launch(tag, '--model', 'fixture/local', success=False)
            self.assertRegex(failure.stderr, 'OpenCode is (not responding|starting)')
            self.assertFalse(self.binding_path('opencode/workspace/' + tag).exists())
            failure = self.hgs(['account', 'start', 'native-opencode'], success=False)
            self.assertRegex(failure.stderr, 'OpenCode is (not responding|starting)')
            self.assertEqual(self.account()['catalog_state'], 'unreachable')
            self.assert_never_started(registration, stand_in)

    def test_09_explicit_start_and_account_catalog(self):
        self.stop_service()
        started = self.hgs(['account', 'start', 'native-opencode'])
        self.assertTrue(self.native_processes())
        self.assertEqual(self.registration()['url'].rstrip('/'), f'http://127.0.0.1:{self.port}')
        until = time.monotonic() + 30
        account = self.account()
        while account['catalog_state'] == 'loading' and time.monotonic() < until:
            time.sleep(.5)
            account = self.account()
        self.assertEqual(account['catalog_state'], 'ready', account)
        self.assertEqual(account['settings_model'], 'fixture/local')
        options = {row['value']: row for row in account['model_options']}
        for value in ('fixture/local', 'fixture/alternate'):
            self.assertEqual(set(options[value]['effort_options']), {'low', 'medium', 'high'})
        self.assertEqual(options['fixture/local']['provider_label'], 'Private keyless fixture')
        self.assertNotIn(f'127.0.0.1:{self.model.server_port}', json.dumps(account))
        self.assert_private(started, account)
        self.assertTrue(account['installed'], account.get('install_reason'))
        self.assertNotIn('install_reason', account)

    def test_10_a_message_removed_while_pending_is_never_resurrected(self):
        b = self.create()
        name, conversation = b['name'], b['conversation_id']
        self.hgs(['send', name, '--json'], self.payload(b, text='HOLD_NATIVE first turn'))
        self.assertTrue(self.model.holding.wait(30), 'the model never received the held turn')
        working = self.wait(name, lambda v: v['phase'] == 'working' and v['interrupt_supported'])
        text = 'Removed before delivery'
        request = self.payload(b, text=text)
        queued = self.hgs(['send', name, '--json'], request)
        self.assertTrue(queued.get('queued'))
        message_id = queued['native_message_id']
        # OpenCode admitted the message, but hgs lost the response: its journal says pending.
        receipt = json.loads(self.receipt_path(request).read_text())
        receipt['status'] = 'pending'
        receipt.pop('queued', None)
        self.receipt_path(request).write_text(json.dumps(receipt))
        self.assertEqual([item['id'] for item in self.inbox(conversation)], [message_id])
        stop = {'request_id': str(uuid.uuid4()), 'expected_run_id': b['run_id'], 'expected_conversation_id': conversation,
                'expected_turn_started': working['turn_started']}
        self.hgs(['interrupt', name, '--json'], stop)
        self.model.release.set()
        self.wait(name, lambda v: v['phase'] == 'idle' and any(e.get('delivery_state') == 'not_delivered' for e in v['events']))
        self.action(b, 'remove_queued', message_id=message_id)
        self.assertEqual(self.inbox(conversation), [])
        # The same request again must not admit the removed message a second time.
        retry = subprocess.run([str(HGS), 'send', name, '--json'], env=self.env, cwd=self.workspace, text=True,
                               capture_output=True, input=json.dumps(request), timeout=60)
        until = time.monotonic() + 5
        while time.monotonic() < until and message_id not in [i for i, _ in self.user_messages(conversation)]:
            time.sleep(.25)
        self.assertNotIn(message_id, [i for i, _ in self.user_messages(conversation)], 'the removed message was delivered')
        self.assertNotIn(text, [t for _, t in self.user_messages(conversation)])
        self.assertEqual(self.inbox(conversation), [])
        self.assertEqual([r for r in self.model.requests if text in r['last_user']], [])
        self.assertNotEqual(retry.returncode, 0, retry.stdout)
        self.assertIn(REMOVED, retry.stderr)
        receipt = json.loads(self.receipt_path(request).read_text())
        self.assertEqual((receipt['status'], receipt['reason'], receipt['native_message_id']),
                         ('refused', REMOVED, message_id))
        self.assertIn(REMOVED, self.hgs(['send', name, '--json'], request, success=False).stderr)
        self.assertEqual(self.inbox(conversation), [])
        self.assertEqual(self.hgs(['inspect', name])['phase'], 'idle')

    def test_11_a_message_left_queued_by_stop_runs_first_with_the_next_message(self):
        # OpenCode keeps a message that Stop left in its queue without running it. The
        # next message starts a run that delivers the leftover message first and then
        # the new one, each with its own reply, ending in one idle. Zerus neither
        # reposts nor refuses the leftover message.
        b = self.create()
        name, conversation = b['name'], b['conversation_id']
        first = self.hgs(['send', name, '--json'], self.payload(b, text='HOLD_NATIVE first turn'))
        self.assertTrue(self.model.holding.wait(30), 'the model never received the held turn')
        working = self.wait(name, lambda v: v['phase'] == 'working' and v['interrupt_supported'])
        left_text, next_text = 'Left in the queue by Stop', 'Sent after Stop'
        left = self.payload(b, text=left_text)
        queued = self.hgs(['send', name, '--json'], left)
        self.assertEqual(queued['status'], 'submitted')
        self.assertTrue(queued.get('queued'))
        left_id = queued['native_message_id']
        stop = {'request_id': str(uuid.uuid4()), 'expected_run_id': b['run_id'], 'expected_conversation_id': conversation,
                'expected_turn_started': working['turn_started']}
        self.hgs(['interrupt', name, '--json'], stop)
        self.model.release.set()
        stopped = self.wait(name, lambda v: v['phase'] == 'idle'
                            and any(e.get('delivery_state') == 'not_delivered' for e in v['events']))
        self.assertEqual([(e['detail'], e['delivery_state']) for e in stopped['events'] if e.get('message_id') == left_id],
                         [(left_text, 'not_delivered')])
        self.assertIn('remove_queued', stopped['allowed_actions'])
        # Left alone, OpenCode keeps the message queued and never runs it.
        until = time.monotonic() + 3
        while time.monotonic() < until:
            self.assertEqual([item['id'] for item in self.inbox(conversation)], [left_id])
            self.assertNotIn(left_id, [i for i, _ in self.user_messages(conversation)])
            time.sleep(.5)
        self.assertEqual([r for r in self.model.requests if left_text in r['last_user']], [])
        receipt = self.receipt_path(left).read_bytes()
        requests_before = len(self.model.requests)
        # The next message, sent while the session is idle, starts a run.
        new = self.payload(b, text=next_text)
        sent = self.hgs(['send', name, '--json'], new)
        self.assertEqual(sent['status'], 'submitted')
        self.assertNotIn('queued', sent)
        next_id = sent['native_message_id']

        def history():
            status, page = self.api('GET', f'/api/session/{conversation}/message?limit=100')
            self.assertEqual(status, 200, page)
            return list(reversed(page['data']))
        until = time.monotonic() + 10
        messages = history()
        while not (next_id in [m['id'] for m in messages] and messages[-1]['type'] == 'idle'):
            if time.monotonic() > until:
                self.fail('OpenCode did not settle after the next message: ' + json.dumps(messages)[-3000:])
            time.sleep(.25)
            messages = history()
        # The leftover message ran first, as its own step with its own reply, then the
        # new one; one run, one idle.
        start = next((i for i, m in enumerate(messages) if m['id'] == left_id), None)
        self.assertIsNotNone(start, 'the leftover message is not in history: ' + json.dumps(messages)[-3000:])
        after = messages[start:]
        self.assertEqual([(m['type'], m['id'] if m['type'] == 'user' else m.get('outcome')) for m in after],
                         [('user', left_id), ('assistant', None), ('user', next_id), ('assistant', None),
                          ('idle', 'succeeded')])
        self.assertEqual([c.get('text') for m in after if m['type'] == 'assistant' for c in m['content']],
                         ['Native OpenCode reply'] * 2)
        self.assertEqual(list(reversed(self.user_messages(conversation))),
                         [(first['native_message_id'], 'HOLD_NATIVE first turn'), (left_id, left_text), (next_id, next_text)])
        self.assertEqual(self.inbox(conversation), [])
        self.assertEqual([r['last_user'] for r in self.model.requests[requests_before:] if r['tools']], [left_text, next_text])
        # Zerus shows both as ordinary delivered prompts, in that order, with nothing to remove.
        view = self.wait(name, lambda v: v['phase'] == 'idle' and not any(e.get('delivery_state') for e in v['events']))
        prompts = [(e['message_id'], e['detail']) for e in view['events'] if e['type'] == 'UserPromptSubmit']
        self.assertEqual(prompts[-2:], [(left_id, left_text), (next_id, next_text)])
        self.assertNotIn('remove_queued', view['allowed_actions'])
        # Zerus never reposted the leftover message and never marked it refused: its
        # receipt is untouched and the same request still answers with its admission.
        self.assertEqual(self.receipt_path(left).read_bytes(), receipt)
        again = self.hgs(['send', name, '--json'], left)
        self.assertEqual((again['status'], again['native_message_id'], again.get('queued')), ('submitted', left_id, True))
        self.assertNotIn('reason', again)
        self.assertEqual(self.receipt_path(left).read_bytes(), receipt)
        # OpenCode answers a same-ID post with the original admission, so also check
        # that no copy under another ID was queued or delivered.
        self.assertEqual(self.inbox(conversation), [])
        users = self.user_messages(conversation)
        self.assertEqual([i for i, _ in users].count(left_id), 1)
        self.assertEqual([t for _, t in users].count(left_text), 1)
        self.assertEqual([r for r in self.model.requests if r['last_user'] == left_text and r['tools']][1:], [])


    def test_12_forget_drops_the_history_cache_and_keeps_native_history(self):
        b = self.create()
        name, conversation = b['name'], b['conversation_id']
        sent = self.hgs(['send', name, '--json'], self.payload(b, text='Cached turn'))
        self.wait(name, lambda v: v['phase'] == 'idle' and any(
            e.get('message_id') == sent['native_message_id'] for e in v['events']))
        cache = self.state / 'opencode/history' / (hashlib.sha256(conversation.encode()).hexdigest() + '.json')
        self.assertTrue(cache.is_file())
        self.action(b, 'forget')
        self.assertFalse(self.binding_path(name).exists())
        self.assertFalse(cache.exists())
        self.assertIn((sent['native_message_id'], 'Cached turn'), self.user_messages(conversation))

    def test_13_a_long_queued_prompt_names_its_request(self):
        # OpenCode 2.0.32 dates a queued message by its delivery: one queued for 12 s was
        # dated 12.0 s after its submission. Phones then cannot match their own Sent
        # message by text and time, so every prompt Zerus sent names its request, while
        # queued and after delivery.
        b = self.create()
        name = b['name']
        self.hgs(['send', name, '--json'], self.payload(b, text='HOLD_NATIVE first turn'))
        self.assertTrue(self.model.holding.wait(30), 'the model never received the held turn')
        self.wait(name, lambda v: v['phase'] == 'working')
        request = self.payload(b, text='Waited in the queue')
        queued = self.hgs(['send', name, '--json'], request)
        self.assertTrue(queued.get('queued'))
        message_id = queued['native_message_id']

        def row(view):
            return next((e for e in view['events'] if e.get('message_id') == message_id), None)
        view = self.wait(name, lambda v: row(v) is not None)
        self.assertEqual((row(view)['delivery_state'], row(view)['request_id']), ('queued', request['request_id']))
        time.sleep(12)
        self.model.release.set()
        view = self.wait(name, lambda v: v['phase'] == 'idle' and row(v) and not row(v).get('delivery_state'))
        self.assertEqual((row(view)['detail'], row(view)['request_id']), ('Waited in the queue', request['request_id']))
        self.assertGreater(row(view)['at'] - queued['submitted_at'], 10)
        # A prompt sent outside Zerus names no request.
        status, _ = self.api('POST', f"/api/session/{b['conversation_id']}/prompt",
                             {'id': 'msg_' + native_suffix(), 'text': 'Typed in OpenCode', 'delivery': 'queue'})
        self.assertLess(status, 300)
        view = self.wait(name, lambda v: v['phase'] == 'idle' and any(e.get('detail') == 'Typed in OpenCode' for e in v['events']))
        self.assertEqual([e.get('request_id') for e in view['events'] if e.get('detail') == 'Typed in OpenCode'], [None])

    def test_20_slash_model_ids_first_then_short_list_and_providers(self):
        account = self.ready_account()
        options = [row['value'] for row in account['model_options']]
        # The slash ID first, through keys and commands older than the provider catalog. The
        # configured siblings are listed, so a missing slash ID fails only this invariant.
        self.assertLessEqual({'fixture/local', 'fixture/alternate'}, set(options), options)
        self.assertIn(SLASH, options, 'SLASH-ID: the configured %s is missing from the short list' % SLASH)
        tag = uuid.uuid4().hex[:10]
        self.launch(tag, '--model', SLASH)
        inspected = self.hgs(['inspect', f'opencode/workspace/{tag}'])
        self.assertEqual(inspected['settings_model'], SLASH)
        # OpenCode stores the model split at the first `/`.
        model = self.session(inspected['conversation_id'])['model']
        self.assertEqual((model['providerID'], model['id']), ('fixture', 'vendor/slash'))
        # Session details pin the session's model with the short list's efforts.
        listed = next(row for row in account['model_options'] if row['value'] == SLASH)
        pinned = next(row for row in inspected['model_options'] if row['value'] == SLASH)
        self.assertEqual(pinned['effort_options'], listed['effort_options'])
        self.assertIn('model_options_truncated', inspected)
        # Only the exact ID is offered: its first two segments name no model.
        refused = self.launch(uuid.uuid4().hex[:10], '--model', 'fixture/vendor', success=False)
        self.assertIn('not offered', refused.stderr)
        # A session changes to the slash ID, and its next turn reaches the model under it.
        b = self.create()
        changed = self.hgs(['settings', b['name'], '--json'], self.payload(b, model=SLASH, effort=''))
        self.assertEqual((changed['status'], changed['model']), ('applied', SLASH))
        model = self.session(b['conversation_id'])['model']
        self.assertEqual((model['providerID'], model['id']), ('fixture', 'vendor/slash'))
        self.hgs(['send', b['name'], '--json'], self.payload(b, text='Slash ID turn'))
        self.wait(b['name'], lambda v: v['phase'] == 'idle' and v.get('reply_id'))
        self.assertEqual({r['model'] for r in self.model.requests if r['tools'] and 'Slash ID turn' in r['last_user']},
                         {'vendor/slash'})
        self.assert_private(inspected, changed)
        # Then the provider catalog's own keys and commands: the default first, keyless free
        # models, provider kinds, and only allowlisted keys whatever OpenCode itself answered.
        self.assertEqual(options[0], 'fixture/local')
        self.assertIs(account['model_options'][0].get('default'), True)
        free = [row for row in account['model_options'] if row['provider'] == 'opencode']
        self.assertTrue(free and all(row['free'] for row in free), account['model_options'])
        kinds = {p['id']: p['kind'] for p in account['providers']}
        self.assertEqual((kinds.get('fixture'), kinds.get('opencode')), ('config', 'free'), account['providers'])
        for provider in account['providers']:
            self.assertLessEqual(set(provider), PROVIDER_KEYS, provider)
        for row in account['model_options']:
            self.assertLessEqual(set(row), ENTRY_KEYS, row)
            self.assertEqual(row['id'], row['value'])
        self.assertIs(account['providers_truncated'], False)
        self.assertLessEqual(len(json.dumps(account, ensure_ascii=True)), 32 * 1024)
        catalog = self.catalog()
        entries = {row['value']: row for row in catalog['models']}
        self.assertEqual(entries[SLASH]['effort_options'], pinned['effort_options'])
        self.assertIs(entries['fixture/local'].get('default'), True)
        self.assertIs(catalog['models_truncated'], False)
        # The full catalog stays within 10,000 models and 4 MiB.
        self.assertLessEqual(len(catalog['models']), 10000)
        self.assertLessEqual(len(json.dumps(catalog).encode()), 4 * 1024 * 1024)
        for row in catalog['models']:
            self.assertLessEqual(set(row), ENTRY_KEYS, row)
        self.assert_private(account, catalog)
        for text in (json.dumps(account), json.dumps(catalog)):
            self.assertNotIn(f'127.0.0.1:{self.model.server_port}', text)
        # The connector gives `hgs account ls` 3 seconds.
        started = time.monotonic()
        self.account()
        self.assertLess(time.monotonic() - started, 3.0)

    def test_21_folder_specific_provider_only_in_its_folder(self):
        self.ready_account()
        self.assert_folder_free(self.catalog())
        folder = self.catalog(self.project)
        provider = next(p for p in folder['providers'] if p['id'] == 'folderonly')
        self.assertEqual((provider.get('folder'), provider['kind']), (True, 'config'))
        self.assertNotIn('folder', next(p for p in folder['providers'] if p['id'] == 'fixture'))
        self.assertIn('folderonly/proj', [m['value'] for m in folder['model_options']])
        self.assertIn('folderonly/proj', [m['value'] for m in folder['models']])
        # The folder's own key and base URL stay in OpenCode too.
        self.assert_private(folder)
        self.assertNotIn(f'127.0.0.1:{self.model.server_port}', json.dumps(folder))
        refused = self.catalog(self.root / 'missing', success=False)
        self.assertIn('Choose an existing folder on this computer', refused.stderr)
        tag = uuid.uuid4().hex[:10]
        self.hgs(['opencode', str(self.project), '--new', '-n', tag, '-d', '--launch-id', str(uuid.uuid4()),
                  '--account', 'native-opencode', '--model', 'folderonly/proj'])
        self.assertEqual(self.hgs(['inspect', f'opencode/project/{tag}'])['settings_model'], 'folderonly/proj')
        # After the same running service read the folder and launched there, the folder's
        # provider still stays out of the home catalog, the account row, its cache and the
        # other folders, and the folder's model is not offered elsewhere.
        row = self.account()
        self.assertEqual(row['catalog_state'], 'ready', row)
        for answer in (self.catalog(), row, self.catalog(self.workspace)):
            self.assert_folder_free(answer)
        self.assertNotIn('folderonly', (self.state / 'opencode/catalog.json').read_text())
        elsewhere = self.launch(uuid.uuid4().hex[:10], '--model', 'folderonly/proj', success=False)
        self.assertIn('not offered', elsewhere.stderr)

    def test_22_not_running_keeps_the_last_catalog_and_never_starts(self):
        (self.root / 'hgs-config/opencode-favorites.json').unlink(missing_ok=True)
        ready = self.ready_account()
        self.stop_service()
        self.assertFalse(self.native_processes())
        account = self.account()
        self.assertEqual(account['catalog_state'], 'not_running')
        self.assertEqual([r['value'] for r in account['model_options']], [r['value'] for r in ready['model_options']])
        self.assertEqual(account['providers'], ready['providers'])
        # The stopped row is the last live catalog, read back from hgs's private cache.
        self.assertEqual(account['catalog_at'], ready['catalog_at'])
        self.assertIn(SLASH, [r['value'] for r in account['model_options']])
        self.assert_folder_free(account)
        catalog = self.catalog()
        self.assertEqual((catalog['catalog_state'], catalog['models']), ('not_running', []))
        self.assertEqual(catalog['model_catalog_source'], 'cache')
        self.assertEqual([r['value'] for r in catalog['model_options']], [r['value'] for r in ready['model_options']])
        folder = self.catalog(self.project)
        self.assertEqual((folder['model_options'], folder['providers']), ([], []))
        # A favorite is hgs's own: no OpenCode file in the private homes or the project changes.
        before = self.opencode_files('home', 'xdg-config', 'xdg-data', 'xdg-state', 'project')
        self.favorite('fixture/alternate', True)
        self.assertEqual(self.opencode_files('home', 'xdg-config', 'xdg-data', 'xdg-state', 'project'), before)
        options = self.account()['model_options']
        self.assertEqual((options[1]['value'], options[1].get('favorite')), ('fixture/alternate', True))
        self.assertFalse(self.native_processes(), 'an inspection or favorite started OpenCode')
        self.assertFalse(self.registration_path().exists())

    def test_23_favorites_reorder_the_short_list_with_request_identity(self):
        favorites = self.root / 'hgs-config/opencode-favorites.json'
        favorites.unlink(missing_ok=True)
        account = self.ready_account()
        configs = [self.root / 'xdg-config/opencode/opencode.json', self.project / 'opencode.json']
        before = [path.read_bytes() for path in configs]
        free = next(r['value'] for r in account['model_options'] if r['provider'] == 'opencode')
        first = str(uuid.uuid4())
        receipt = self.favorite(free, True, first)
        self.assertEqual((receipt['status'], receipt['favorite']), ('applied', True))
        options = self.account()['model_options']
        self.assertEqual((options[1]['value'], options[1].get('favorite')), (free, True))
        # The identity is bound to its change: another model or the other flag under it is
        # refused and changes nothing, neither the stored list nor the first receipt.
        stored = favorites.read_bytes()
        record = self.state / 'opencode/favorite-requests' / (first + '.json')
        kept = record.read_bytes()
        for model, on in (('fixture/alternate', True), (free, False)):
            refused = self.favorite(model, on, first, success=False)
            self.assertIn(FAVORITE_CONFLICT, refused.stderr)
        self.assertEqual((favorites.read_bytes(), record.read_bytes()), (stored, kept))
        self.assertEqual([r['value'] for r in self.account()['model_options']], [r['value'] for r in options])
        # After on and off, the first identity replays its receipt without applying again.
        self.favorite(free, False)
        replay = self.favorite(free, True, first)
        self.assertEqual(replay['status'], 'applied')
        self.assertNotIn(free, replay['favorites'])
        self.assertFalse(any(r.get('favorite') for r in self.account()['model_options']))
        self.favorite('gone/missing', True)
        catalog = self.catalog()
        self.assertIn({'value': 'gone/missing', 'available': False}, catalog['favorites'])
        self.assertNotIn('gone/missing', [r['value'] for r in self.account()['model_options']])
        # Favorites live beside the hgs account catalog, never in OpenCode's configuration.
        self.assertEqual(json.loads(favorites.read_text()), {'version': 1, 'models': ['gone/missing']})
        self.assertEqual([path.read_bytes() for path in configs], before)

    def test_25_catalog_reads_never_ask_for_credentials(self):
        # hgs reads the registration from XDG_STATE_HOME. This test's hgs commands read a
        # copy that names a recording loopback proxy in front of the real private service;
        # the service and its own registration stay untouched.
        (self.root / 'hgs-config/opencode-favorites.json').unlink(missing_ok=True)
        self.ready_account()
        b = self.create()
        registration = self.registration()
        upstream = registration['url'].rstrip('/')
        seen, answers = [], []
        # The proxy forwards straight to the private service, never through a proxy of its own.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        class Proxy(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def forward(self):
                seen.append((self.command, self.path))
                length = int(self.headers.get('Content-Length') or 0)
                headers = {k: v for k, v in self.headers.items() if k.lower() in ('authorization', 'content-type')}
                request = urllib.request.Request(upstream + self.path, method=self.command, headers=headers,
                                                 data=self.rfile.read(length) if length else None)
                try:
                    with opener.open(request, timeout=30) as response:
                        status, kind, raw = response.status, response.headers.get('Content-Type'), response.read()
                except urllib.error.HTTPError as error:
                    with error:
                        status, kind, raw = error.code, error.headers.get('Content-Type'), error.read()
                answers.append(raw)
                self.send_response(status)
                self.send_header('Content-Type', kind or 'application/json')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
            do_GET = do_POST = do_PATCH = do_PUT = do_DELETE = forward

        proxy = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
        threading.Thread(target=proxy.serve_forever, daemon=True).start()
        self.addCleanup(proxy.server_close)
        self.addCleanup(proxy.shutdown)
        state = self.root / 'proxy-state'
        (state / 'opencode').mkdir(parents=True, exist_ok=True)
        descriptor = os.open(state / 'opencode/service.json', os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, 'w') as file:
            json.dump({**registration, 'url': f'http://127.0.0.1:{proxy.server_port}'}, file)
        # An instance attribute: stop_service and every other test keep the class environment.
        self.env = {**self.env, 'XDG_STATE_HOME': str(state)}
        outputs = [self.account(), self.catalog(), self.catalog(self.project)]
        catalog_reads = list(seen)
        outputs.append(self.hgs(['inspect', b['name']]))
        reads = len(seen)
        # Favorites, including a refused reuse of an identity, never talk to OpenCode.
        reused = str(uuid.uuid4())
        outputs += [self.favorite('fixture/alternate', True, reused), self.favorite('fixture/alternate', False)]
        self.assertIn(FAVORITE_CONFLICT, self.favorite('fixture/local', True, reused, success=False).stderr)
        self.assertEqual(len(seen), reads, 'a favorite talked to OpenCode')
        routes = {path.split('?')[0] for _, path in catalog_reads}
        # The reads did go through the proxy.
        self.assertTrue({'/api/provider', '/api/model', '/api/model/default', '/api/integration'} <= routes, seen)
        self.assertEqual({method for method, _ in seen}, {'GET'}, seen)
        self.assertEqual([path for _, path in seen if path.startswith('/api/credential')], [], seen)
        self.assertLessEqual(routes, CATALOG_ROUTES, catalog_reads)
        self.assert_private(*outputs)
        self.assertNotIn(f'127.0.0.1:{self.model.server_port}', json.dumps(outputs))
        # The canary and the base URL did reach hgs: OpenCode answered with them and hgs dropped them.
        self.assertTrue(any(FIXTURE_KEY.encode() in raw for raw in answers), 'the canary never reached hgs')
        self.assertTrue(any(f'127.0.0.1:{self.model.server_port}'.encode() in raw for raw in answers))

if __name__ == '__main__':
    unittest.main()
