"""DeepSeek hosts that hgs starts give their agents the hgs hint as a system
prompt section of the hgs bridge (src/dsh/bridge.mjs). DeepSeekHint checks that
against the released harness, sealed so that it cannot change a shared install
or reach a live host; SealingHelpers checks its seals and fixtures without any
harness or host, and always runs.

What a host runs, from a static reading of @deepseek-ai/dsh 0.2.0-rc.2:

- `dsh` is a symlink to the package's lib/bin.js, a `#!/usr/bin/env node`
  script. The process image is therefore node (nodejs 26.11.1 on the reviewed
  machine), which loads system libraries from /usr/lib, and the package is
  data that node reads. DeepSeekHint copies node and the whole package, starts
  `NODE_COPY PACKAGE_COPY/lib/bin.js` without a PATH lookup, and requires
  /proc/PID/exe to be the node copy, its command line to name the package copy
  and each mapped file to lie in the copy, in the test's own folder or in an
  administrator-owned system library whose bytes stay the same.
- The package holds 11 symbolic links, all relative node_modules/.bin links
  inside it. A link that leaves the tree, or a device, socket or FIFO, refuses
  the copy. Native addons (sherpa-onnx, sharp, node-pty and
  node-addon-require-builtin) load from the copy.
- Children: the shell tools start bash or SHELL from PATH, their sandbox starts
  bin/landlock-run, and `open` starts xdg-open without --no-open. The keyless
  model calls no tool, so the host may start no process at all.
- Updates: the package has no self-update path. The web bundle's plugin
  manager reaches a registry only when asked to install a plugin, and
  DSH_TELEMETRY_DISABLED=1 turns telemetry off.
- Environment: lib/bin.js adds <cwd>/.env and <DSH_HOME>/.env beneath the
  inherited variables, and the home file may set HTTP_PROXY, HTTPS_PROXY,
  ALL_PROXY and NO_PROXY. Both files must be absent. A host gets exactly the
  HOST_ENV variables built here, none inherited: no other HGS_*, DSH_*, XDG_*,
  NODE_*, LD_*, proxy, provider or key variable. Its sockets may only reach
  loopback addresses. This class talks to the bridge socket itself, never
  through hgs, so no hgs command can start the shared `dsh` on a dead host.
- The system prompt: the agent loop commits the rendered prompt as a
  system/message event at the first node of the session and keeps it there.
  A changed prompt replaces that node on a route without systemPromptUpdate
  and follows the history on an in-history route, as DeepSeek's own route
  declares. The model fixture reports every system message and the effective
  one, so new, resumed and opted-out sessions are judged by what the model
  reads.

DeepSeekHint runs only with HGS_NATIVE_HINT_FIXTURES=1, after review, with the
reviewed digests of the shared package tree (HGS_DSH_HINT_TREE_SHA256) and of
node (HGS_DSH_HINT_NODE_SHA256). It refuses to start anything when the shared
install differs, and fails when any shared or copied byte changes.
"""
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
import uuid

REPO = Path(__file__).resolve().parents[1]
FIXTURES = REPO / 'tests/fixtures'
VERSION = '0.2.0-rc.2'
# src/state/agent_hint.rs HINT, as tests/test_agent_hint.py pins it.
HINT = ('This agent runs in a Zerus session: the hgs CLI reads, messages, launches and stops other agent '
        'sessions on this and other computers. Run hgs guide before using it.')
SYSTEM_PATH = '/usr/bin:/bin'
HOST_ENV = ('PATH', 'LANG', 'HOME', 'DSH_HOME', 'DSH_TELEMETRY_DISABLED', 'HGS_DSH_TEST_PACKAGE')


def file_digest(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as file:
        for chunk in iter(lambda: file.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def tree_digest(root):
    """The tree's SHA-256 over names, kinds, permission bits, file bytes and
    link targets. A link that leaves the tree, or any other special file,
    refuses it."""
    root = Path(root).resolve()
    digest = hashlib.sha256()
    for directory, folders, files in os.walk(root):
        folders.sort()
        for name in sorted(folders + files):
            path = Path(directory) / name
            relative, info = path.relative_to(root).as_posix(), path.lstat()
            if stat.S_ISLNK(info.st_mode):
                target = os.readlink(path)
                if not (path.parent / target).resolve().is_relative_to(root):
                    raise ValueError(f'link leaves the tree: {relative} -> {target}')
                record = ['l', relative, target]
            elif stat.S_ISDIR(info.st_mode):
                record = ['d', relative, stat.S_IMODE(info.st_mode)]
            elif stat.S_ISREG(info.st_mode):
                record = ['f', relative, stat.S_IMODE(info.st_mode), file_digest(path)]
            else:
                raise ValueError(f'special file: {relative}')
            digest.update(json.dumps(record).encode() + b'\n')
    return digest.hexdigest()


def copy_tree(source, target):
    """`source` copied to the new folder `target`, with the same digest."""
    digest = tree_digest(source)
    shutil.copytree(source, target, symlinks=True)
    if tree_digest(target) != digest:
        raise ValueError('the copy differs from its source')
    return digest


def host_env(root, package):
    """The whole environment of a host, built rather than inherited."""
    return {'PATH': SYSTEM_PATH, 'LANG': 'C.UTF-8', 'HOME': str(root / 'user'), 'DSH_HOME': str(root / 'native'),
            'DSH_TELEMETRY_DISABLED': '1', 'HGS_DSH_TEST_PACKAGE': str(package)}


def process_image(pid):
    """What process `pid` runs: executable, arguments, folder, initial
    environment and every mapped file."""
    proc = Path('/proc') / str(pid)
    environ = (proc / 'environ').read_bytes().split(b'\0')
    maps = set()
    for line in (proc / 'maps').read_text().splitlines():
        fields = line.split(None, 5)
        if len(fields) == 6 and fields[5].startswith('/'):
            maps.add(fields[5].removesuffix(' (deleted)'))
    return {'exe': os.readlink(proc / 'exe'), 'cwd': os.readlink(proc / 'cwd'),
            'cmdline': [arg.decode() for arg in (proc / 'cmdline').read_bytes().split(b'\0')[:-1]],
            'environ': dict(item.decode().split('=', 1) for item in environ if b'=' in item), 'maps': sorted(maps)}


def system_file(path):
    """A system file that only the administrator can change."""
    try:
        info = os.stat(path)
    except OSError:
        return False
    return path.startswith(('/usr/', '/lib')) and info.st_uid == 0 and not info.st_mode & 0o022


def descendants(pid):
    """Every live process below `pid`."""
    children = {}
    for entry in Path('/proc').iterdir():
        try:
            fields = (entry / 'stat').read_text().rsplit(')', 1)[1].split()
        except (OSError, IndexError, ValueError):
            continue
        if entry.name.isdigit():
            children.setdefault(int(fields[1]), []).append(int(entry.name))
    found, queue = [], [pid]
    while queue:
        for child in children.get(queue.pop(), []):
            found.append(child)
            queue.append(child)
    return sorted(found)


def loopback(address):
    """Whether a /proc/net remote address (hex host:port) is loopback or unset."""
    host = address.split(':')[0]
    raw = b''.join(bytes.fromhex(host[i:i + 8])[::-1] for i in range(0, len(host), 8))
    ip = ipaddress.ip_address(raw)
    mapped = ip.version == 6 and ip.ipv4_mapped
    return ip.is_loopback or ip.is_unspecified or bool(mapped and mapped.is_loopback)


def remotes(pid):
    """The remote addresses of the sockets that process `pid` holds."""
    inodes = set()
    for fd in (Path('/proc') / str(pid) / 'fd').iterdir():
        try:
            target = os.readlink(fd)
        except OSError:
            continue
        if target.startswith('socket:['):
            inodes.add(target[8:-1])
    found = []
    for table in ('tcp', 'tcp6', 'udp', 'udp6'):
        for line in (Path('/proc') / str(pid) / 'net' / table).read_text().splitlines()[1:]:
            fields = line.split()
            if fields[9] in inodes:
                found.append(fields[2])
    return found


NODE_CHECK = r'''
import { pathToFileURL } from 'node:url';
const [census, model, log] = process.argv.slice(1);
const { census: count } = await import(pathToFileURL(census));
const system = text => ({ role: 'system', source: { kind: 'system-prompt' }, content: text === '' ? [] : [{ type: 'text', text }] });
const user = text => ({ role: 'user', source: { kind: 'user' }, content: [{ type: 'text', text }] });
const hint = 'HINT', results = {};
results.leading = count([system('a HINT'), user('hi')], hint, false);
results.history = count([system('a HINT'), user('hi'), system('a'), user('again')], hint, true);
results.emptyTail = count([system('a HINT'), user('hi'), system('')], hint, true);
results.leadingOnlyIgnoresLater = count([system('a'), user('hi'), system('a HINT')], hint, false);
results.quoted = count([system('a'), user('HINT HINT')], hint, false);
results.none = count([user('hi')], hint, true);
const { apply } = await import(pathToFileURL(model));
let adapter;
apply({ llm: { registerAdapter: (ids, value) => { results.ids = ids; adapter = value; } } }, { hint, inHistory: true, log });
results.resolved = await adapter.resolveModel('hgs-hint', 'hint-a');
results.events = [];
for await (const event of adapter.stream({ messages: [system('a HINT'), user('turn')] })) results.events.push(event.type);
for await (const event of adapter.stream({ system: 'title HINT', messages: [user('turn')] })) results.events.push(event.type);
console.log(JSON.stringify(results));
'''


class SealingHelpers(unittest.TestCase):
    """The seals and fixtures of DeepSeekHint, in a private folder and
    environment; no harness, host or model runs."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='hgs-dsh-hint-check-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    @unittest.skipIf(os.name == 'nt', 'POSIX links and FIFOs')
    def test_a_tree_digest_covers_bytes_modes_and_links_and_refuses_escapes(self):
        tree = self.root / 'tree'
        (tree / 'lib/bin').mkdir(parents=True)
        (tree / 'lib/bin.js').write_text('#!/usr/bin/env node\n')
        (tree / 'lib/bin/tool').write_text('tool')
        os.symlink('../bin.js', tree / 'lib/bin/link')
        digest = tree_digest(tree)
        self.assertEqual(copy_tree(tree, self.root / 'copy'), digest)
        self.assertEqual(tree_digest(self.root / 'copy'), digest)
        for change in (lambda: (tree / 'lib/bin/tool').write_text('Tool'),
                       lambda: (tree / 'lib/bin/tool').chmod(0o700),
                       lambda: ((tree / 'lib/bin/link').unlink(), os.symlink('tool', tree / 'lib/bin/link'))):
            change()
            self.assertNotEqual(tree_digest(tree), digest)
            digest = tree_digest(tree)
        os.symlink('../../..', tree / 'lib/out')
        with self.assertRaisesRegex(ValueError, 'leaves the tree'):
            tree_digest(tree)
        (tree / 'lib/out').unlink()
        os.mkfifo(tree / 'lib/fifo')
        with self.assertRaisesRegex(ValueError, 'special file'):
            tree_digest(tree)

    def test_a_host_environment_is_built_not_inherited(self):
        polluted = {'HGS_SESSION': 'x/y', 'HGS_STATE_DIR': '/x', 'DSH_HOME': '/x', 'HTTPS_PROXY': 'http://x.invalid',
                    'NODE_OPTIONS': '--require /x.js', 'LD_PRELOAD': '/x.so', 'DEEPSEEK_API_KEY': 'k', 'XDG_CONFIG_HOME': '/x'}
        with mock.patch.dict(os.environ, polluted):
            env = host_env(self.root, self.root / 'package')
        self.assertEqual(tuple(env), HOST_ENV)
        self.assertEqual((env['PATH'], env['DSH_HOME'], env['HOME']),
                         (SYSTEM_PATH, str(self.root / 'native'), str(self.root / 'user')))
        self.assertFalse(set(polluted.values()) & set(env.values()))

    @unittest.skipUnless(sys.platform == 'linux', 'Linux /proc')
    def test_a_process_image_names_its_executable_environment_and_children(self):
        env = {'PATH': SYSTEM_PATH, 'LANG': 'C.UTF-8'}
        argv = [sys.executable, '-I', '-c', 'import time; time.sleep(30)']
        child = subprocess.Popen(argv, env=env, cwd=self.root, stdin=subprocess.DEVNULL, start_new_session=True)
        self.addCleanup(child.wait)
        self.addCleanup(child.kill)
        # Until its exec completes, the child is a copy of this interpreter or
        # has an empty image; read it again once its arguments show.
        deadline = time.monotonic() + 10
        while process_image(child.pid)['cmdline'] != argv and time.monotonic() < deadline:
            time.sleep(.01)
        image = process_image(child.pid)
        self.assertEqual(image['exe'], os.path.realpath(sys.executable))
        self.assertEqual((image['environ'], image['cwd']), (env, str(self.root)))
        self.assertIn(image['exe'], image['maps'])
        self.assertIn(child.pid, descendants(os.getpid()))
        self.assertEqual(descendants(child.pid), [])
        self.assertEqual(remotes(child.pid), [])
        for address, expected in (('0100007F:1F90', True), ('00000000:0000', True), ('0101A8C0:0050', False),
                                  ('00000000000000000000000001000000:0050', True),
                                  ('0000000000000000FFFF00000100007F:0050', True),
                                  ('B80D0120000000000000000001000000:0050', False)):
            self.assertEqual(loopback(address), expected, address)

    @unittest.skipUnless(shutil.which('node', path=SYSTEM_PATH), 'node runs the fixture modules')
    def test_the_census_and_model_fixtures_report_where_the_hint_is(self):
        # A stand-in for the one harness module the model imports.
        package = self.root / 'package'
        llm = package / 'node_modules/@deepseek-ai/dsh-llm'
        llm.mkdir(parents=True)
        (package / 'package.json').write_text('{"name": "stand-in"}')
        (llm / 'package.json').write_text('{"name": "@deepseek-ai/dsh-llm", "type": "module", "exports": "./index.js"}')
        (llm / 'index.js').write_text('export class LlmAdapter {}\n')
        log = self.root / 'requests.jsonl'
        env = {'PATH': SYSTEM_PATH, 'LANG': 'C.UTF-8', 'HOME': str(self.root), 'HGS_DSH_TEST_PACKAGE': str(package)}
        result = subprocess.run([shutil.which('node', path=SYSTEM_PATH), '--input-type=module', '-e', NODE_CHECK,
                                 str(FIXTURES / 'dsh-hint-census.mjs'), str(FIXTURES / 'dsh-hint-model.mjs'), str(log)],
                                env=env, cwd=self.root, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        results = json.loads(result.stdout)
        self.assertEqual(results['leading'], {'system': [1], 'other': 0, 'leading': True, 'effective': 1})
        self.assertEqual(results['history'], {'system': [1, 0], 'other': 0, 'leading': True, 'effective': 0})
        self.assertEqual(results['emptyTail']['effective'], 1)
        self.assertEqual(results['leadingOnlyIgnoresLater']['effective'], 0)
        self.assertEqual((results['quoted']['other'], results['quoted']['effective']), (2, 0))
        self.assertEqual(results['none'], {'system': [], 'other': 0, 'leading': False, 'effective': 0})
        self.assertEqual(results['ids'], ['hgs-hint'])
        self.assertEqual(results['resolved']['systemPromptUpdate'], 'in-history')
        self.assertEqual(results['events'], ['block-start', 'text-delta', 'block-end', 'usage', 'finish'] * 2)
        loop, side = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual((loop['prompt'], loop['slot'], loop['effective']), ('turn', None, 1))
        self.assertEqual((side['slot'], side['effective']), (1, 0))


@unittest.skipUnless(sys.platform == 'linux', 'the process-image proof reads Linux /proc')
@unittest.skipUnless(os.environ.get('HGS_NATIVE_HINT_FIXTURES') == '1',
                     'native hint fixtures run only with HGS_NATIVE_HINT_FIXTURES=1 after review')
class DeepSeekHint(unittest.TestCase):
    """New, resumed and opted-out sessions of a private copied host read the
    hint exactly once, or not at all when opted out."""

    @classmethod
    def setUpClass(cls):
        shared = os.environ.get('HGS_DSH_TEST_BIN') or shutil.which('dsh')
        node = shutil.which('node')
        pinned = (os.environ.get('HGS_DSH_HINT_TREE_SHA256', ''), os.environ.get('HGS_DSH_HINT_NODE_SHA256', ''))
        if not shared or not node or not all(pinned):
            raise unittest.SkipTest('needs dsh, node and the reviewed HGS_DSH_HINT_*_SHA256 digests')
        entry = Path(shared).resolve()
        cls.shared, cls.shared_node = entry.parents[1], Path(node).resolve()
        manifest = json.loads((cls.shared / 'package.json').read_text())
        if (manifest.get('name'), manifest.get('version'), manifest.get('bin'), entry.relative_to(cls.shared).as_posix()) != (
                '@deepseek-ai/dsh', os.environ.get('HGS_DSH_TEST_VERSION', VERSION), {'dsh': 'lib/bin.js'}, 'lib/bin.js'):
            raise RuntimeError('the shared dsh is not the reviewed release; nothing was started')
        if not entry.read_bytes().startswith(b'#!/usr/bin/env node\n'):
            raise RuntimeError('the shared dsh entry is no longer a node script; nothing was started')
        cls.digests = (tree_digest(cls.shared), file_digest(cls.shared_node))
        if cls.digests != pinned:
            raise RuntimeError('the shared install differs from the reviewed one; nothing was started')
        # One verified private copy per reviewed digest, never the shared files.
        cls.copy = REPO / 'target/hint-evidence/dsh' / pinned[0][:16]
        cls.node, cls.package = cls.copy / 'node', cls.copy / 'package'
        if not cls.copy.exists():
            cls.copy.mkdir(parents=True, mode=0o700)
            shutil.copy2(cls.shared_node, cls.node)
            copy_tree(cls.shared, cls.package)
        if (tree_digest(cls.package), file_digest(cls.node)) != pinned:
            raise RuntimeError('the private copy changed; it was left for inspection')
        cls.temp = tempfile.TemporaryDirectory(prefix='hgs-dsh-hint-', dir='/tmp')
        cls.root = Path(cls.temp.name)
        cls.workspace = cls.root / 'workspace'
        for folder in (cls.workspace, cls.root / 'user', cls.root / 'native', cls.root / 'recovery'):
            folder.mkdir(mode=0o700)
        cls.env = host_env(cls.root, cls.package)
        cls.socket, cls.log = cls.root / 'host.sock', cls.root / 'requests.jsonl'
        cls.libraries, cls.evidence = {}, {'digests': pinned, 'steps': []}

    @classmethod
    def tearDownClass(cls):
        try:
            cls.evidence['libraries'] = cls.libraries
            stamp = time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
            (cls.copy.parent / f'evidence-{stamp}.json').write_text(json.dumps(cls.evidence, indent=1))
            changed = [path for path, digest in cls.libraries.items() if file_digest(path) != digest]
            if changed or (tree_digest(cls.shared), file_digest(cls.shared_node)) != cls.digests:
                raise RuntimeError(f'shared files changed during the run: {changed or "the dsh package or node"}')
            if (tree_digest(cls.package), file_digest(cls.node)) != cls.digests:
                raise RuntimeError('the private copy changed during the run; it was left for inspection')
        finally:
            cls.temp.cleanup()

    def overlay(self, hint, in_history):
        # The bridge settings are bridge_config's (src/state/dsh.rs), with or without the hint.
        bridge = {'socket': str(self.socket), 'recoveryPolicy': str(self.root / 'recovery/policy.json')}
        if hint:
            bridge['agentHint'] = HINT
        patch = [
            {'id': 'llm-deepseek', 'disabled': True},
            {'id': 'agent-default-model', 'config': {'provider': 'hgs-hint', 'model': 'hint-a'}},
            {'insert': [
                {'id': 'hgs-hint-model', 'name': str(FIXTURES / 'dsh-hint-model.mjs'),
                 'config': {'hint': HINT, 'inHistory': in_history, 'log': str(self.log)}},
                {'id': 'hgs-native-dsh', 'name': str(REPO / 'src/dsh/bridge.mjs'), 'config': bridge},
            ]},
        ]
        path = self.root / f'patch-{uuid.uuid4()}.json'
        path.write_text(json.dumps(patch))
        return path

    def start(self, hint, in_history):
        for folder in (self.workspace, Path(self.env['DSH_HOME'])):
            self.assertFalse((folder / '.env').exists(), folder)
        with (self.root / 'host.log').open('a') as log:
            host = subprocess.Popen([str(self.node), str(self.package / 'lib/bin.js'), '--profile', 'web',
                                     '--patch', str(self.overlay(hint, in_history)), '--no-open',
                                     '--host', '127.0.0.1', '--port', '0'], env=self.env, cwd=self.workspace,
                                    stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        self.addCleanup(self.stop, host)
        deadline = time.monotonic() + 40
        while not self.socket.exists() and host.poll() is None and time.monotonic() < deadline:
            time.sleep(.1)
        self.assertTrue(self.socket.exists(), (self.root / 'host.log').read_text()[-2500:])
        self.prove(host)
        return host

    def prove(self, host):
        image = process_image(host.pid)
        self.assertEqual(image['exe'], str(self.node))
        self.assertEqual(image['cmdline'][:2], [str(self.node), str(self.package / 'lib/bin.js')])
        self.assertEqual((image['environ'], image['cwd']), (self.env, str(self.workspace)))
        for path in image['maps']:
            if not (Path(path).is_relative_to(self.copy) or Path(path).is_relative_to(self.root)):
                self.assertTrue(system_file(path), path)
                self.libraries.setdefault(path, file_digest(path))
        self.assertEqual(descendants(host.pid), [])
        self.assertTrue(all(loopback(address) for address in remotes(host.pid)), remotes(host.pid))

    def stop(self, host):
        try:
            if host.poll() is None:
                self.prove(host)
        finally:
            if host.poll() is None:
                os.killpg(host.pid, signal.SIGTERM)
                try:
                    host.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(host.pid, signal.SIGKILL)
                    host.wait(timeout=5)
        if self.socket.exists():
            self.socket.unlink()
        # Nothing the host started outlives it.
        left = []
        for entry in Path('/proc').iterdir():
            try:
                if entry.name.isdigit() and str(self.root).encode() in (entry / 'cmdline').read_bytes():
                    left.append(int(entry.name))
            except OSError:
                pass
        self.assertEqual(left, [])

    def rpc(self, method, params):
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(30)
            client.connect(str(self.socket))
            client.sendall((json.dumps({'method': method, 'params': params}) + '\n').encode())
            response = json.loads(client.makefile().readline())
        self.assertTrue(response['ok'], response.get('error', response))
        return response['value']

    def turn(self, step, session, host):
        """The census of the agent loop's request for one typed turn."""
        prompt = f'HINT_FIXTURE_{step.upper()}_{session[-12:]}'
        self.rpc('send', {'sessionId': session, 'requestId': str(uuid.uuid4()),
                          'content': [{'type': 'text', 'text': prompt}], 'mode': 'steer'})
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            entries = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
            loop = [entry for entry in entries if entry['prompt'] == prompt and entry['slot'] is None]
            rows = self.rpc('list', {}).get('items', [])
            if loop and any(row.get('sessionId') == session and row.get('running') is False for row in rows):
                self.prove(host)
                self.evidence['steps'].append({'step': step, **loop[-1]})
                return loop[-1]
            time.sleep(.1)
        self.fail(f'{step}: the turn did not finish')

    def stored(self):
        """Files under the native home, outside its profiles, that hold the hint."""
        home = Path(self.env['DSH_HOME'])
        return sorted(path.relative_to(home).as_posix() for path in home.rglob('*')
                      if path.is_file() and 'profiles' not in path.relative_to(home).parts
                      and HINT.encode() in path.read_bytes())

    def sessions(self, in_history):
        host = self.start(True, in_history)
        session = 'session-' + str(uuid.uuid4())
        self.rpc('create', {'sessionId': session, 'cwd': str(self.workspace), 'title': 'hint fixture'})
        for step in ('new', 'second'):
            census = self.turn(step, session, host)
            self.assertEqual((census['effective'], census['other'], sum(census['system'])), (1, 0, 1), step)
        self.stop(host)
        self.evidence['steps'].append({'step': 'stored', 'files': self.stored()})
        for step, hint, expected in (('resumed', True, 1), ('opted-out', False, 0), ('enabled-again', True, 1)):
            host = self.start(hint, in_history)
            self.rpc('resume', {'sessionId': session})
            census = self.turn(step, session, host)
            self.assertEqual((census['effective'], census['other']), (expected, 0), step)
            if step == 'resumed':
                self.assertEqual(sum(census['system']), 1, step)
            self.stop(host)

    def test_an_in_history_route_reads_the_hint_once_per_effective_prompt(self):
        self.sessions(True)

    def test_a_leading_only_route_reads_the_hint_once_per_effective_prompt(self):
        self.sessions(False)


if __name__ == '__main__':
    unittest.main()
