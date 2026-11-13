"""Adapter controls; the native suites separately exercise the real engine."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

BRIDGE = Path(__file__).resolve().parents[1] / 'src/dsh/bridge.mjs'
LIST_CAPTURE = Path(__file__).resolve().parent / 'fixtures/dsh-bridge-list-row.json'
# Rewrites LIST_CAPTURE from the bridge's own answers instead of comparing them.
REGENERATE_LIST_CAPTURE = 'ZERUS_REGENERATE_DSH_BRIDGE_CAPTURE'


@unittest.skipUnless(shutil.which('node'), 'Node.js required')
class ModelSelection(unittest.TestCase):
    def run_js(self, body):
        script = """
import assert from 'node:assert/strict';
const { modelSelectionAdapter: adapt, completedForkCut, matchesForkCut,
        scheduleSession, manageSchedule } = await import(process.argv[2]);
const route = { provider: 'fixture', model: 'selected', reasoningEffort: 'off' };
const control = () => ({
  composeAgent: async () => ({ setup() {}, agentPreset: 'standard' }),
  presetForObservation() {}, serializeImageAdmission() {},
  selectionFor() { throw Error('read installed a selection'); },
  selectModel() { throw Error('profile default changed'); },
});
""" + body
        result = subprocess.run(['node', '--input-type=module', '-', BRIDGE.as_uri()],
                                input=script, text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_legacy_reads_never_install_selection_and_keep_native_composition(self):
        self.run_js("""
const c = control(), composition = { setup() {}, agentPreset: 'original' };
c.composeAgent = async preset => { assert.equal(preset, 'chosen'); return composition; };
c.selections = new WeakMap();
const writes = [];
c.selectForNextRequest = (agent, value) => writes.push([agent, value]);
const m = adapt(c), agent = { options: { provider: 'fixture', model: 'inherited' } };
assert.equal(await m.compose('chosen'), composition);
assert.equal(m.has(agent), false);
assert.deepEqual(m.current(agent), agent.options);
assert.deepEqual(m.current(agent, { modelSelection: { next: route } }), route);
assert.equal(c.selections.has(agent), false);
c.selections.set(agent, { current: route });
assert.equal(m.has(agent), true);
const read = m.current(agent); read.model = 'changed copy';
assert.deepEqual(m.current(agent), route);
m.select(agent, route);
assert.deepEqual(writes, [[agent, route]]);
assert.deepEqual(agent.options, { provider: 'fixture', model: 'inherited' });
""")

    def test_agent_model_reads_and_writes_stay_on_that_original_agent(self):
        self.run_js("""
const c = control(), m = adapt(c), writes = [];
const agent = { options: { provider: 'fixture', model: 'inherited' }, model: {
  current() { assert.equal(this, agent.model); return route; },
  select(value) { assert.equal(this, agent.model); writes.push(value); },
} };
assert.equal(m.has(agent), true);
const read = m.current(agent, { modelSelection: { next: { model: 'stale' } } });
read.model = 'changed copy';
assert.deepEqual(m.current(agent), route);
assert.deepEqual(writes, []);
m.select(agent, route);
assert.deepEqual(writes, [route]);
assert.equal(agent.options.model, 'inherited');
assert.equal(m.current({ model: { current() {}, select() {} } }), null);
""")

    def test_new_model_is_checked_before_native_setup_and_publication(self):
        self.run_js("""
const c = control(), context = {}, calls = [];
const agent = { model: { current() {}, select() {} } };
const composition = { agentPreset: 'chosen', setup(ctx, original) {
  assert.equal(this, composition); calls.push([ctx, original]); return 'native setup';
} };
c.composeAgent = async () => composition;
const wrapped = await adapt(c).compose('chosen');
assert.equal(wrapped.agentPreset, 'chosen');
assert.throws(() => wrapped.setup(context, {}), /incompatible official/);
assert.deepEqual(calls, []);
assert.equal(wrapped.setup(context, agent), 'native setup');
assert.deepEqual(calls, [[context, agent]]);
""")

    def test_partial_controller_and_missing_image_serializer_refuse(self):
        self.run_js("""
for (const key of ['composeAgent', 'presetForObservation', 'serializeImageAdmission']) {
  const c = control(); delete c[key];
  assert.throws(() => adapt(c), /incompatible official/);
}
for (const partial of [
  { selections: new WeakMap() }, { selectForNextRequest() {} },
  { selections: new Map(), selectForNextRequest() {} },
]) assert.throws(() => adapt({ ...control(), ...partial }), /incompatible official/);
const c = control(); c.composeAgent = async () => ({});
await assert.rejects(adapt(c).compose('standard'), /incompatible official/);
""")

    def test_missing_agent_model_never_falls_back_to_stored_or_default_route(self):
        self.run_js("""
const m = adapt(control());
for (const model of [undefined, {}, { current() {} }, { select() {} }]) {
  const agent = { model, options: route };
  assert.throws(() => m.current(agent, { modelSelection: { next: route } }), /incompatible official/);
  assert.throws(() => m.select(agent, route), /incompatible official/);
  assert.throws(() => m.has(agent), /incompatible official/);
}
""")

    def test_new_fork_requires_the_exact_completed_source_prefix_and_seed_marker(self):
        self.run_js("""
const events = [
  { seq: 0, type: 'user/message', data: { content: 'original' } },
  { seq: 1, type: 'turn/end', data: {} },
  { seq: 2, type: 'user/message', data: { content: 'later unfinished turn' } },
];
const cut = completedForkCut({ events });
assert.equal(cut.atSeq, 1);
const original = { header: { isSeeded: true }, inheritedEventCount: 2,
  events: [...events.slice(0, 2), { seq: 2, type: 'session/end-seed', data: { inherited: true } }] };
assert.equal(matchesForkCut(original, cut), true);
for (const mutate of [
  x => x.header.isSeeded = false,
  x => x.header.parentSession = 'unrelated',
  x => x.inheritedEventCount = 1,
  x => x.events[0].data.content = 'another source',
  x => x.events[0].seq = 7,
  x => x.events[2].type = 'user/message',
  x => x.events[2].data.inherited = false,
  x => x.events[2].seq = 3,
]) {
  const changed = structuredClone(original); mutate(changed);
  assert.equal(matchesForkCut(changed, cut), false);
}
assert.throws(() => completedForkCut({ events: events.slice(0, 1) }), /completed reply/);
""")

    def test_new_schedules_bind_only_the_destination_and_preserve_native_cas(self):
        self.run_js("""
const entry = { id: 'task', kind: 'every', title: 'Before', prompt: 'Check',
  target: { kind: 'session', sessionId: 'original' }, involvedSessionIds: ['editor'],
  status: 'active', updatedAt: '2026-11-05T00:00:00.000Z' };
assert.equal(scheduleSession(entry), 'original');
assert.equal(scheduleSession({ sessionId: 'legacy' }), 'legacy');
assert.equal(scheduleSession({ ...entry, target: { kind: 'new' } }), undefined);
assert.equal(scheduleSession({ ...entry, sessionId: 'editor', target: null }), undefined);
const edits = [], histories = [];
const schedule = { async view({ id }) { assert.equal(id, 'task'); return entry; },
  async edit(request) { edits.push(request); return { ...entry, ...request.patch }; },
  async history(request) { histories.push(request); return { records: [
    { messageId: 'ours', targetSessionId: 'original' }, { messageId: 'foreign', targetSessionId: 'editor' },
  ], nextBefore: 'opaque' }; } };
const expected = { ...entry }; delete expected.status;
const p = { id: 'task', sessionId: 'original', action: 'update', expected, title: 'After' };
assert.equal((await manageSchedule(schedule, p)).updated, true);
assert.deepEqual(edits, [{ id: 'task', action: 'update', expectedUpdatedAt: entry.updatedAt, patch: { title: 'After' } }]);
const conflict = await manageSchedule(schedule, { ...p, expected: { ...expected, title: 'stale' } });
assert.equal(conflict.code, 'schedule_conflict'); assert.equal(edits.length, 1);
for (const action of ['history', 'delete', 'update']) {
  const result = await manageSchedule(schedule, { ...p, sessionId: 'editor', action, limit: 10 });
  assert.equal(result.code, 'schedule_not_found');
}
assert.deepEqual(histories, []); assert.equal(edits.length, 1);
const history = await manageSchedule(schedule, { ...p, action: 'history', limit: 10, before: 'cursor' });
assert.deepEqual(history, { records: [{ messageId: 'ours', targetSessionId: 'original' }], nextBefore: 'opaque' });
assert.deepEqual(histories, [{ id: 'task', limit: 10, before: 'cursor' }]);
// Retargeting inside the native queue after view must remain a CAS refusal.
schedule.edit = async request => {
  assert.equal(request.expectedUpdatedAt, entry.updatedAt);
  return { id: request.id, code: 'schedule_conflict' };
};
assert.equal((await manageSchedule(schedule, p)).updated, false);
assert.equal((await manageSchedule(schedule, { ...p, action: 'delete' })).code, 'schedule_conflict');
schedule.history = async () => {
  entry.target = { kind: 'session', sessionId: 'editor' };
  return { records: [{ messageId: 'foreign', targetSessionId: 'editor' }] };
};
assert.equal((await manageSchedule(schedule, { ...p, action: 'history', limit: 10 })).code, 'schedule_not_found');
""")

    def test_legacy_schedules_keep_the_original_session_scoped_methods(self):
        self.run_js("""
const calls = [], schedule = {};
for (const action of ['update', 'delete', 'history']) schedule[action] = async request => {
  calls.push([action, request]); return 'original result';
};
const p = { id: 'task', sessionId: 'original', expected: { id: 'task' }, title: 'After' };
assert.equal(await manageSchedule(schedule, { ...p, action: 'update' }), 'original result');
await manageSchedule(schedule, { ...p, action: 'delete' });
await manageSchedule(schedule, { ...p, action: 'history', limit: 10 });
assert.deepEqual(calls, [
  ['update', { sessionId: 'original', id: 'task', expected: p.expected, title: 'After' }],
  ['delete', { sessionId: 'original', id: 'task' }],
  ['history', { sessionId: 'original', id: 'task', limit: 10 }],
]);
await assert.rejects(manageSchedule({ update() {}, view() {}, edit() {} }, { ...p, action: 'delete' }), /incompatible official/);
""")


# The real bridge's apply() over a minimal stand-in for the official host: a
# job registry, a scripted session list and no-op hooks. It answers ping and
# list on its own socket, exactly as HGS reads them.
LIST_HARNESS = r"""
import net from 'node:net';
const [bridgeUrl, socket, withRegistry] = process.argv.slice(2);
const { apply } = await import(bridgeUrl);
const job = (id, owner, status, extra = {}) =>
  ({ id, kind: 'bash', owner, status, startedAt: 1790000000000, label: `sleep ${id}`, ...extra });
const jobs = [
  job('job-1', 'session-busy', 'running'),
  job('job-2', 'session-busy', 'completed', { finishedAt: 1790000060000 }),
  { ...job('job-3', 'session-busy', 'running'), kind: 'agent' },
  job('job-4', 'session-child', 'running'),
  ...Array.from({ length: 33 }, (_, i) => job(`many-${i}`, 'session-many', 'running')),
];
const registry = { list: owner => jobs.filter(entry => entry.owner === owner), events: { subscribe: () => () => {} } };
const rows = [
  { sessionId: 'session-busy', running: false, agentAvailable: true },
  { sessionId: 'session-child', origin: 'subagent', parentSessionId: 'session-busy', running: true, agentAvailable: true },
  { sessionId: 'session-idle', running: false, agentAvailable: true },
  { sessionId: 'session-many', running: false, agentAvailable: true },
];
const effects = [];
const ctx = new Proxy({
  sessionController: { list: async () => ({ items: rows.map(row => ({ ...row })) }),
    agents: { composeAgent: async () => ({}), presetForObservation() {}, serializeImageAdmission() {} } },
  agents: { get: () => undefined, roots: () => [] },
  goals: { get: () => null },
  get: name => (name === 'jobs' && withRegistry === '1' ? registry : undefined),
  on: () => () => {},
  effect: run => { effects.push(run()); },
}, { get: (target, key) => (key in target ? target[key] : undefined) });
apply(ctx, { socket });
await Promise.all(effects);
const call = method => new Promise((resolve, reject) => {
  const client = net.connect(socket);
  let data = '';
  client.on('data', chunk => { data += chunk; });
  client.on('end', () => resolve(JSON.parse(data)));
  client.on('error', reject);
  client.write(JSON.stringify({ method, params: {} }) + '\n');
});
process.stdout.write(JSON.stringify({ ping: await call('ping'), list: await call('list') }));
process.exit(0);
"""


@unittest.skipUnless(shutil.which('node'), 'Node.js required')
class ListJobs(unittest.TestCase):
    """zerus-yj7ef: the list rows' bounded `listJobs` summary (docs/reference.md)."""

    def answers(self, registry=True):
        with tempfile.TemporaryDirectory(prefix='hgs-dsh-list-', dir=os.environ.get('ZERUS_TEST_SHORT_TMPDIR', '/tmp')) as root:
            result = subprocess.run(['node', '--input-type=module', '-', BRIDGE.as_uri(), str(Path(root) / 'host.sock'),
                                     '1' if registry else '0'], input=LIST_HARNESS, text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        answers = json.loads(result.stdout)
        for answer in answers.values():
            self.assertTrue(answer['ok'], answer)
            # The host generation is a fresh UUID per bridge; nothing else varies.
            answer['value']['generation'] = '<generation>'
        return {name: answer['value'] for name, answer in answers.items()}

    def test_rows_count_running_jobs_only_when_complete(self):
        answers = self.answers()
        self.assertIs(answers['ping']['listJobs'], True)
        rows = {row['sessionId']: row['listJobs'] for row in answers['list']['items']}
        # Own and listed subagents' running bash jobs; settled and other kinds are not work.
        self.assertEqual((rows['session-busy']['complete'], rows['session-busy']['count']), (True, 2))
        self.assertEqual([job['id'] for job in rows['session-busy']['jobs']], ['job-1', 'job-4'])
        self.assertEqual(rows['session-child'], {**rows['session-child'], 'complete': True, 'count': 1})
        self.assertEqual(rows['session-idle'], {'complete': True, 'count': 0, 'jobs': []})
        # Over the bound, or without a registry, there is no count: unknown, never idle.
        self.assertEqual(rows['session-many'], {'complete': False, 'count': None, 'jobs': []})
        bare = self.answers(registry=False)
        self.assertEqual({row['listJobs']['complete'] for row in bare['list']['items']}, {False})

    def test_list_answer_matches_the_committed_capture(self):
        answers = self.answers()
        capture = {'about': 'The real bridge\'s ping and list answers for the zerus-yj7ef listJobs contract, '
                            'written by tests/test_dsh_bridge.py ListJobs. Regenerate: '
                            f'{REGENERATE_LIST_CAPTURE}=1 python3 -m unittest test_dsh_bridge.ListJobs (from tests/).',
                   **answers}
        if os.environ.get(REGENERATE_LIST_CAPTURE) == '1':
            LIST_CAPTURE.write_text(json.dumps(capture, indent=2, sort_keys=True) + '\n')
        self.assertEqual(json.loads(LIST_CAPTURE.read_text()), capture,
                         f'The bridge\'s list answer changed; review it, then set {REGENERATE_LIST_CAPTURE}=1')


@unittest.skipUnless(shutil.which('node'), 'Node.js required')
class ShellIdentity(unittest.TestCase):
    """zerus-ppd26: each tool call's processes carry HGS_AGENT=dsh and exactly
    the calling session's own markers, or none of them."""

    def run_js(self, body):
        script = """
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { AsyncLocalStorage } from 'node:async_hooks';
const { callEnvironment, parseIdentity, identitySession, readIdentity, wrapSpawns } = await import(process.argv[2]);
const conversation = 'session-91250d76-ca68-48b6-87f8-3a0e4400c314';
const file = { version: 1, conversation, session: 'dsh/project/a', run: 'run-1',
               session_id: '6a4c5887-557a-4f31-b1c8-ad0a94c3c705', account: 'native-dsh' };
const markers = ['HGS_SESSION', 'HGS_RUN_ID', 'HGS_SESSION_ID', 'HGS_ACCOUNT_ID'];
""" + body
        result = subprocess.run(['node', '--input-type=module', '-', BRIDGE.as_uri()],
                                input=script, text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_call_carries_its_own_markers_or_removes_all_of_them(self):
        self.run_js("""
const none = callEnvironment(undefined);
assert.equal(none.HGS_AGENT, 'dsh');
// Explicit undefined entries are the subprocess service's tombstones.
for (const key of markers) { assert.ok(Object.hasOwn(none, key), key); assert.equal(none[key], undefined); }
assert.ok(Object.isFrozen(none));
assert.deepEqual({ ...callEnvironment(parseIdentity(JSON.stringify(file), conversation)) }, {
  HGS_AGENT: 'dsh', HGS_SESSION: 'dsh/project/a', HGS_RUN_ID: 'run-1',
  HGS_SESSION_ID: '6a4c5887-557a-4f31-b1c8-ad0a94c3c705', HGS_ACCOUNT_ID: 'native-dsh' });
""")

    def test_only_a_complete_identity_of_the_exact_conversation_is_accepted(self):
        self.run_js("""
assert.deepEqual(parseIdentity(JSON.stringify(file), conversation),
  { session: 'dsh/project/a', run: 'run-1', session_id: file.session_id, account: 'native-dsh' });
const other = 'session-00000000-0000-4000-8000-000000000001';
for (const text of ['', 'not json', 'null', JSON.stringify({ ...file, version: 2 }),
    JSON.stringify({ ...file, conversation: other }), JSON.stringify({ ...file, session: '' }),
    JSON.stringify({ ...file, run: 7 }), JSON.stringify({ ...file, session_id: undefined }),
    JSON.stringify({ ...file, account: 'a'.repeat(2049) }), JSON.stringify({ ...file, session: 'a' + String.fromCharCode(10) + 'b' }),
    JSON.stringify({ ...file, session: 'a' + String.fromCharCode(0x85) })])
  assert.equal(parseIdentity(text, conversation), undefined, text);
""")

    def test_a_child_call_carries_its_top_level_sessions_identity(self):
        self.run_js("""
const parent = conversation, child = 'session-00000000-0000-4000-8000-000000000002';
assert.equal(identitySession({ session: { id: parent, header: { id: parent, origin: 'user' } } }), parent);
assert.equal(identitySession({ session: { id: child, header: { id: child, origin: 'subagent', parentSession: parent } } }), parent);
for (const agent of [undefined, {}, { session: { id: '../escape', header: {} } },
    { session: { id: child, header: { origin: 'subagent', parentSession: 'webhook-x' } } }])
  assert.equal(identitySession(agent), undefined);
""")

    def test_the_identity_is_read_bounded_without_following_a_link(self):
        self.run_js("""
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'hgs-identity-'));
try {
  const target = path.join(dir, conversation + '.json');
  fs.writeFileSync(target, JSON.stringify(file));
  assert.equal(readIdentity(dir, conversation).run, 'run-1');
  assert.equal(readIdentity(null, conversation), undefined);
  assert.equal(readIdentity(dir, undefined), undefined);
  fs.writeFileSync(target, JSON.stringify({ ...file, run: 'r'.repeat(5000) }));
  assert.equal(readIdentity(dir, conversation), undefined);
  fs.rmSync(target);
  assert.equal(readIdentity(dir, conversation), undefined);
  fs.writeFileSync(path.join(dir, 'real.json'), JSON.stringify(file));
  fs.symlinkSync(path.join(dir, 'real.json'), target);
  assert.equal(readIdentity(dir, conversation), undefined);
} finally { fs.rmSync(dir, { recursive: true, force: true }); }
""")

    def test_processes_started_during_a_call_carry_its_environment(self):
        self.run_js("""
const store = new AsyncLocalStorage(), seen = [];
const service = { spawn(spec) { seen.push(['spawn', spec]); return spec; },
                  spawnTerminal(spec) { seen.push(['terminal', spec]); return spec; } };
const original = service.spawn;
const wrap = wrapSpawns(service, () => store.getStore());
assert.equal(wrap.installed, true);
assert.ok(wrap.current());
const outside = { argv: ['true'], env: { HGS_SESSION: 'caller' } };
assert.equal(service.spawn(outside), outside);
const env = callEnvironment(undefined);
await store.run(env, async () => {
  await new Promise(resolve => setTimeout(resolve, 1));
  const spec = service.spawn({ argv: ['bash'], env: { NO_COLOR: '1', HGS_SESSION: 'inherited', HGS_AGENT: 'claude' } });
  assert.deepEqual(spec.env, { NO_COLOR: '1', ...env });
  assert.equal(service.spawnTerminal({ argv: ['bash'] }).env.HGS_AGENT, 'dsh');
});
assert.equal(seen.length, 3);
service.spawn = function replaced() {};
assert.equal(wrap.current(), false);
wrap.restore();
assert.equal(service.spawn.name, 'replaced');
const fresh = { spawn: original };
const again = wrapSpawns(fresh, () => env);
again.restore();
assert.equal(fresh.spawn, original);
assert.equal(wrapSpawns({}, () => env).installed, false);
""")


if __name__ == '__main__':
    unittest.main()
