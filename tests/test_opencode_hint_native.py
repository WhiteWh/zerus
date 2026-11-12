"""Latest-official OpenCode context plugin contract, in its own private database."""
try:
    import fcntl
except ImportError:
    fcntl = None
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import shutil
import subprocess
import sys
import time
import unittest
import uuid

import test_opencode_native as native


class OpenCodeHintNative(native.OpenCodeNative):
    @classmethod
    def cleanup(cls):
        if hasattr(cls, 'env'):
            cls.stop_service()
        # Keep an unexpectedly occupied folder for review, rather than remove
        # it. Only this fixture's own private service is stopped above.
        users = []
        if hasattr(cls, 'root') and Path('/proc').is_dir():
            for entry in Path('/proc').iterdir():
                if not entry.name.isdigit() or int(entry.name) == os.getpid():
                    continue
                try:
                    cwd = (entry / 'cwd').resolve(strict=True)
                    if cwd == cls.root or cls.root in cwd.parents:
                        users.append(int(entry.name))
                except OSError:
                    pass
        elif hasattr(cls, 'root') and sys.platform == 'darwin':
            # BSD ps has no cwd column. Read only cwd names; retain only users
            # of our random private folder, without logging other paths.
            result = subprocess.run(['/usr/sbin/lsof', '-n', '-P', '-a', '-d', 'cwd',
                                     '-u', str(os.getuid()), '-Fpn'], capture_output=True,
                                    text=True, timeout=10)
            if result.returncode not in (0, 1):
                cls.temp._finalizer.detach()
                raise RuntimeError('Private fixture cwd verification failed')
            pid = None
            for line in result.stdout.splitlines():
                if line.startswith('p'):
                    pid = line[1:]
                elif line.startswith('n') and (line[1:] == str(cls.root) or line[1:].startswith(str(cls.root)+'/')):
                    users.append(pid)
        if users:
            cls.temp._finalizer.detach()
            raise RuntimeError('Private fixture still has cwd users: '+str(users))
        super().cleanup()

    def test_existing_turn_context_hint(self):
        config_path = self.root / 'xdg-config/opencode/opencode.json'
        # A person's existing plugin and settings must survive the launch overlay.
        owner = self.root / 'owner-plugin'
        owner.mkdir()
        (owner / 'package.json').write_text(json.dumps({
            'type': 'module', 'name': 'owner-canary', 'exports': {'./server': './index.js'}}))
        (owner / 'index.js').write_text('export default {id:"owner-canary",setup:async(ctx)=>{'
            'await ctx.session.hook("context",e=>{e.system.push({type:"text",text:"OWNER_PLUGIN_CANARY"})})}};')
        config = json.loads(config_path.read_text())
        config['plugins'] = [str(owner)]
        config_path.write_text(json.dumps(config, indent=2) + '\n')
        original_config = config_path.read_bytes()
        original_plugin = (owner / 'index.js').read_bytes()

        def messages(binding):
            status, page = self.api('GET', f"/api/session/{binding['conversation_id']}/message?limit=100")
            self.assertEqual(status, 200, page)
            self.assertNotIn(native.Model.HINT, json.dumps(page))
            self.assertNotIn('Instructions updated', json.dumps(page))
            return page['data']

        def wait(binding):
            status, answer = self.api('POST', f"/api/experimental/session/{binding['conversation_id']}/wait", {})
            self.assertEqual(status, 204, answer)

        def turn(binding, text, expected):
            receipt = self.hgs(['send', binding['name'], '--json'], self.payload(binding, text=text))
            self.assertNotIn(native.Model.HINT, json.dumps(receipt))
            wait(binding)
            requests = [r for r in self.model.requests if r['last_user'] == text and r['tools']]
            self.assertEqual(len(requests), 1, (text, self.model.requests[-2:]))
            model_messages = requests[0]['messages']
            actual = json.dumps(model_messages).count(native.Model.HINT)
            if actual != expected:
                marker = json.loads((self.state / 'opencode/context-hint/service.json').read_text())
                probe = self.hgs(['--opencode-agent-hint', marker['nonce'], str(marker['pid']),
                                  str(self.workspace), binding['conversation_id']])
                detail = {'text': text, 'expected': expected, 'actual': actual, 'helper_after': probe,
                          'record': json.loads(self.binding_path(binding['name']).read_text()), 'marker': marker,
                          'model_requests': self.model.requests}
                detail['plugins'] = self.api('GET', '/api/plugin?location%5Bdirectory%5D='+native.urllib.parse.quote(str(self.workspace), safe=''))
                procenv = Path('/proc') / str(marker['pid']) / 'environ'
                if procenv.exists():
                    detail['service_overlay'] = [v.decode() for v in procenv.read_bytes().split(b'\0')
                                                 if v.startswith(b'OPENCODE_CONFIG_CONTENT=')]
                if folder := os.environ.get('ZERUS_TEST_OPENCODE_HINT_EVIDENCE'):
                    (Path(folder) / 'context-native-failure.json').write_text(json.dumps(detail, indent=2)+'\n')
                self.fail(json.dumps({k:v for k,v in detail.items() if k not in ('model_requests','plugins','service_overlay')}))
            self.assertEqual(json.dumps(model_messages).count('OWNER_PLUGIN_CANARY'), 1)
            self.assertTrue(all(native.Model.HINT not in json.dumps(m)
                                for m in model_messages if m['role'] != 'system'))
            return messages(binding)

        def resume(binding):
            self.hgs(['opencode', str(self.workspace), '-n', binding['name'].rsplit('/', 1)[1], '-d',
                      '--account', 'native-opencode'])
            return json.loads(self.binding_path(binding['name']).read_text())

        def create_used(tag, parent=None):
            conversation = 'ses_' + native.native_suffix()
            body = {'id': conversation, 'title': tag}
            body.update({'parentID': parent} if parent else {'location': {'directory': str(self.workspace)}})
            status, answer = self.api('POST', '/api/session', body)
            self.assertEqual(status, 200, answer)
            binding = self.save_binding(tag, service='opencode2', conversation_id=conversation)
            turn(binding, tag + ' before adoption', 0)
            return binding

        # A service started outside Zerus gets no overlay and is never restarted.
        self.opencode('service', 'start')
        external = create_used('external')
        pid = self.registration()['pid']
        unavailable = resume(external)
        self.assertEqual(self.registration()['pid'], pid)
        self.assertEqual(unavailable['agent_hint_context']['state'], 'unavailable')
        self.assertEqual(self.hgs(['inspect', external['name']])['agent_hint_status']['state'], 'unavailable')
        turn(external, 'Unmanaged existing conversation', 0)
        self.stop_service()  # Only this test's private native service.

        # The production cold-start path supplies the service-only overlay.
        self.hgs(['account', 'start', 'native-opencode'])
        target = create_used('target')
        other = create_used('unrelated')
        baseline = messages(target)
        target = resume(target)
        self.assertEqual(target['agent_hint_context']['state'], 'ready', target)
        after = turn(target, 'After existing turn adoption', 1)
        byid = {m['id']: m for m in after}
        self.assertTrue(all(byid[row['id']] == row for row in baseline))
        turn(other, 'Unrelated conversation', 0)
        target = resume(target)
        turn(target, 'Repeated resume', 1)
        # An older real service can still activate its source after another
        # registration overwrites Zerus's latest marker. Give that service its
        # own HOME/config/state and retain its exact launch-overlay source.
        marker_path = self.state / 'opencode/context-hint/service.json'
        first_marker = json.loads(marker_path.read_text())
        first_folder = marker_path.parent / 'plugins' / first_marker['nonce']
        # A start whose service identity was never verified stays unknown.
        failed_folder = first_folder.parent / str(uuid.uuid4())
        shutil.copytree(first_folder, failed_folder)
        ownership = json.loads((failed_folder / 'owner.json').read_text())
        ownership.update(nonce=failed_folder.name, launch='starting')
        ownership.pop('service', None)
        (failed_folder / 'owner.json').write_text(json.dumps(ownership))
        alternate = self.root / 'older-live-service'
        alt_env = dict(self.env)
        for key, folder in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'), ('XDG_STATE_HOME', 'state'),
                            ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache')]:
            path = alternate / folder
            path.mkdir(parents=True, mode=0o700)
            alt_env[key] = str(path)
        (alternate / 'config/opencode').mkdir(mode=0o700)
        shutil.copyfile(config_path, alternate / 'config/opencode/opencode.json')
        def alt(*args, hgs=False):
            run = subprocess.run([str(native.HGS) if hgs else native.NATIVE, *args], env=alt_env, cwd=self.workspace,
                                 stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
            self.assertEqual(run.returncode, 0, run.stderr[-1000:])
            return run

        def stop_primary():
            pid = self.registration()['pid']
            self.opencode('service', 'stop')
            end = time.monotonic() + 10
            while any(p == pid for p, _ in self.native_processes()) and time.monotonic() < end:
                time.sleep(.025)
            self.assertFalse(any(p == pid for p, _ in self.native_processes()))

        alt('service', 'set', 'port', str(native.free_port()))
        try:
            alt('account', 'start', 'native-opencode', hgs=True)
            registration = alternate / 'state/opencode/service.json'
            end = time.monotonic() + 10
            while not registration.exists() and time.monotonic() < end:
                time.sleep(.025)
            alt_reg = json.loads(registration.read_text())
            self.assertNotEqual(alt_reg['pid'], self.registration()['pid'])
            old_marker = json.loads(marker_path.read_text())
            old_folder = marker_path.parent / 'plugins' / old_marker['nonce']
            self.assertNotEqual(old_folder, first_folder)
            old_owner = json.loads((old_folder / 'owner.json').read_text())
            self.assertEqual(old_owner['service']['pid'], alt_reg['pid'])
            self.assertTrue(old_owner['service']['birth'])
            # Two primary restarts overwrite the latest marker. The older
            # independently managed service is protected by its own folder pin.
            for attempt in range(2):
                stop_primary()
                target = resume(target)
                self.assertTrue(old_folder.joinpath('index.js').is_file())
            self.assertFalse(first_folder.exists())
            self.assertTrue(failed_folder.exists())
            self.assertNotEqual(json.loads(marker_path.read_text())['nonce'], old_marker['nonce'])
            headers = {}
            if alt_reg.get('password'):
                token = native.base64.b64encode(('opencode:' + alt_reg['password']).encode()).decode()
                headers['Authorization'] = 'Basic ' + token
            query = native.urllib.parse.urlencode({'location[directory]': str(self.workspace)})
            request = native.urllib.request.Request(alt_reg['url'].rstrip('/') + '/api/plugin?' + query, headers=headers)
            end = time.monotonic() + 5
            while True:
                with native.urllib.request.urlopen(request, timeout=2) as response:
                    plugins = json.load(response)['data']
                active = [p for p in plugins if p['id'] == 'zerus.agent-hint.context'
                          and p['source']['path'] == str(old_folder) and p['state']['status'] == 'active']
                if active or time.monotonic() >= end:
                    break
                time.sleep(.025)
            self.assertEqual(len(active), 1, plugins)
        finally:
            alt('service', 'stop')
        for attempt in range(3):
            self.stop_service()
            target = resume(target)
            if not old_folder.exists():
                break
        self.assertFalse(old_folder.exists())
        turn(target, 'Real service restart', 1)
        start = len(self.model.requests)
        status, answer = self.api('POST', f"/api/session/{target['conversation_id']}/compact", {})
        self.assertEqual(status, 200, answer)
        wait(target)
        compacted = [r for r in messages(target) if r['type'] == 'compaction']
        self.assertEqual([r['status'] for r in compacted], ['completed'], compacted)
        status, active_context = self.api('GET', f"/api/session/{target['conversation_id']}/context")
        self.assertEqual(status, 200, active_context)
        self.assertNotIn('target before adoption', json.dumps(active_context))
        self.assertTrue(self.model.requests[start:])
        for r in self.model.requests[start:]:
            self.assertNotIn(native.Model.HINT, json.dumps(r['messages']))
        turn(target, 'After native compaction', 1)
        self.env['HGS_AGENT_HINT_OFF'] = 'native-opencode'
        target = resume(target)
        turn(target, 'Decided off', 0)
        del self.env['HGS_AGENT_HINT_OFF']
        # A contradictory service environment cannot override the decided record.
        target = resume(target)
        turn(target, 'Re-enabled', 1)
        renamed = 'opencode/workspace/context-renamed'
        receipt = self.hgs(['session-action', target['name'], '--json'],
                           self.payload(target, action='rename', new_name=renamed))
        self.assertEqual(receipt['status'], 'completed', receipt)
        target = json.loads(self.binding_path(renamed).read_text())
        turn(target, 'After rename', 1)

        marker_path = self.state / 'opencode/context-hint/service.json'
        marker = json.loads(marker_path.read_text())
        args = ['--opencode-agent-hint', marker['nonce'], str(marker['pid']), str(self.workspace), target['conversation_id']]
        self.env['HGS_AGENT_HINT'] = '0'
        self.env['HGS_AGENT_HINT_OFF'] = 'native-opencode'
        self.assertEqual(self.hgs(args)['context'], native.Model.HINT)
        del self.env['HGS_AGENT_HINT']
        del self.env['HGS_AGENT_HINT_OFF']
        # A constant number of bounded routes: malformed, stale, absent and busy
        # records all omit; none starts native work or replays any message.
        before_requests = len(self.model.requests)
        route = self.state / 'opencode/context-hint/bindings' / (hashlib.sha256(target['conversation_id'].encode()).hexdigest()+'.json')
        stale = json.loads(self.binding_path(target['name']).read_text())
        stale['context_hint_test_churn'] = True
        for path, bad in [(route, b'{}'), (route, b' ' * 16385),
                          (self.binding_path(target['name']), b'{}'),
                          (self.binding_path(target['name']), json.dumps(stale, sort_keys=True, separators=(',', ':')).encode()),
                          (marker_path, b'{}')]:
            original = path.read_bytes()
            try:
                path.write_bytes(bad)
                self.assertIsNone(self.hgs(args)['context'])
            finally:
                path.write_bytes(original)
        for lock_path in [self.state / '.lock', self.state / 'opencode/bindings.lock']:
            with lock_path.open('a') as held:
                fcntl.flock(held, fcntl.LOCK_EX)
                started = time.monotonic()
                self.assertIsNone(self.hgs(args)['context'])
                self.assertLess(time.monotonic()-started, .75)
                fcntl.flock(held, fcntl.LOCK_UN)
        self.assertEqual(len(self.model.requests), before_requests)
        # Native subagents remain excluded even if an explicit binding exists.
        child = create_used('native-child', parent=target['conversation_id'])
        resume(child)
        turn(child, 'Bound native child', 0)
        # Competing decided bindings for one native conversation have no sole
        # entitlement. The private route must omit, never use the last writer.
        competing = self.save_binding('competing', service='opencode2', conversation_id=target['conversation_id'])
        resume(competing)
        turn(target, 'Conflicting decided bindings', 0)
        # A retired sole binding loses its route. A competing/ambiguous route
        # must remain blocked instead of making another holder the winner.
        other_route = self.state / 'opencode/context-hint/bindings' / (hashlib.sha256(other['conversation_id'].encode()).hexdigest()+'.json')
        other = resume(other)
        self.assertTrue(other_route.exists())
        self.action(other, 'forget')
        self.assertFalse(other_route.exists())
        self.action(competing, 'forget')
        self.assertTrue(route.exists())
        self.assertIsNone(self.hgs(args)['context'])
        self.action(target, 'forget')
        self.assertFalse(route.exists())
        self.assertEqual(config_path.read_bytes(), original_config)
        self.assertEqual((owner / 'index.js').read_bytes(), original_plugin)
        self.assertFalse((self.root / 'xdg-config/opencode/plugins').exists())
        self.assertFalse((self.root / 'xdg-config/opencode/plugin').exists())
        self.stop_service()
        # This suite owns a fresh database, so retained revision-1 fixtures in
        # the ordinary native suite cannot mask persistence in another table.
        databases = list(self.root.rglob('*.db'))
        self.assertTrue(databases)
        table_count = 0
        for db in databases:
            with sqlite3.connect(f'file:{db}?mode=ro', uri=True) as con:
                for (name,) in con.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall():
                    table_count += 1
                    quoted = '"' + name.replace('"', '""') + '"'
                    for row in con.execute('SELECT * FROM '+quoted):
                        self.assertNotIn(native.Model.HINT, json.dumps(row, default=str), (name, row))
        self.assertGreater(table_count, 0)
        if folder := os.environ.get('ZERUS_TEST_OPENCODE_HINT_EVIDENCE'):
            out = Path(folder)
            out.mkdir(parents=True, exist_ok=True)
            (out / 'context-native.json').write_text(json.dumps({
                'version': self.version, 'model_requests': self.model.requests, 'database_tables_scanned': table_count,
                'target': target, 'baseline': baseline, 'config_unchanged': True, 'owner_plugin_unchanged': True,
                'compaction': compacted, 'compacted_active_context': active_context,
                'hint_in_database': False, 'unmanaged_service_pid_preserved': pid,
                'cleanup': {'older_live_service_pid': alt_reg['pid'], 'older_source_activated': True,
                            'older_source_pruned_after_exit': not old_folder.exists(),
                            'unknown_start_folder_retained': failed_folder.exists(),
                            'known_dead_first_service_folder_pruned': not first_folder.exists(),
                            'sole_binding_route_retired': not other_route.exists(),
                            'ambiguous_route_blocked_until_last_holder_forgotten': True},
            }, indent=2)+'\n')


def load_tests(loader, _tests, _pattern):
    # Reuse the private service fixture, not the parent's contract inventory.
    return loader.loadTestsFromName('OpenCodeHintNative.test_existing_turn_context_hint', sys.modules[__name__])
