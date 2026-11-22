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
        scheduleSession, manageSchedule, compactAvailable, compactCommand, blockingQuestion } = await import(process.argv[2]);
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

    def test_blocking_question_requires_owned_root_foreground_and_no_person_intent(self):
        self.run_js("""
const agent = {}, request = {agent, questions:[{id:'q',question:'Pick one'}]};
assert.equal(blockingQuestion(request,agent),true);
assert.equal(blockingQuestion({...request,wait:{callId:'tool-1'}},agent),true);
assert.equal(blockingQuestion({...request,wait:{callId:'tool-1',timed:false}},agent),true);
assert.equal(blockingQuestion(request,{}),false);
assert.equal(blockingQuestion({...request,agent:undefined},undefined),false);
for(const wait of [null,{},[],{callId:''},{callId:'tool-1',timed:true},{callId:'tool-1',timed:null},{callId:'tool-1',timed:'false'}]) {
  assert.equal(blockingQuestion({...request,wait},agent),false,JSON.stringify(wait));
}
for(const intent of [{kind:'approval'},{kind:'plan-review'},{kind:'trust'},{},null]) {
  assert.equal(blockingQuestion({...request,questions:[{...request.questions[0],intent}]},agent),false);
}
for(const questions of [[],null,{},[null],[[]],[{}],[{id:"",question:"?"}]])assert.equal(blockingQuestion({...request,questions},agent),false);
""")

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

    def test_compaction_is_the_native_seam_and_not_only_the_slash_command(self):
        self.run_js("""
const compact = { definitionId: '@deepseek-ai/dsh-command-compact', name: 'compact' };
const other = { definitionId: '@deepseek-ai/dsh-command-goal', name: 'goal' };
const seam = { async compactNow() {} };
// The slash command registers in an interactive composition; the seam is the
// native engine both run. A headless host may have only the seam.
assert.equal(compactCommand(compact), compact);
assert.equal(compactCommand(other), undefined);
assert.equal(compactCommand(undefined), undefined);
assert.equal(compactAvailable(compact, undefined), true);
assert.equal(compactAvailable(undefined, seam), true);
assert.equal(compactAvailable(other, seam), true);
assert.equal(compactAvailable(undefined, undefined), false);
assert.equal(compactAvailable(other, {}), false);
assert.equal(compactAvailable(undefined, { compactNow: 'not a method' }), false);
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


# Run the real bridge over a private socket, holding only the admitted native
# resume. A second session lets us distinguish a fence from a busy refusal.
RESUME_STATUS_HARNESS = r"""
import assert from 'node:assert/strict';
import net from 'node:net';
const [bridgeUrl, socket] = process.argv.slice(2);
const { apply } = await import(bridgeUrl);
const admitted = 'session-11111111-1111-4111-8111-111111111111';
const unseen = 'session-22222222-2222-4222-8222-222222222222';
let enter, release;
const entered = new Promise(resolve => { enter = resolve; });
const held = new Promise(resolve => { release = resolve; });
const resumes = [], effects = [];
apply({
  sessionController: { agents: {
    composeAgent: async () => ({ setup() {} }), presetForObservation() {}, serializeImageAdmission() {},
  } },
  sessionQuery: { observeSession: async () => ({
    header: { origin: 'main', cwd: process.cwd() }, [Symbol.dispose]() {},
  }) },
  agentDefaultModel: { currentSelection: () => ({ provider: 'fixture', model: 'fixture' }) },
  workspaceRegistry: { create: async path => ({ path, sessionIds: [], async attachSession() {} }) },
  agents: { get: () => undefined, resume: async ({ resumeSessionId, setup }) => {
    resumes.push(resumeSessionId);
    if (resumeSessionId === admitted) {
      enter();
      await held;
    }
    const agent = { session: { id: resumeSessionId }, model: { current() {}, select() {} } };
    setup({}, agent);
    return { agent, dispose: async () => {} };
  } },
  get: () => undefined,
  on: () => () => {},
  effect: run => effects.push(run()),
}, { socket });
const cleanup = await Promise.all(effects);

async function call(method, params) {
  const raw = await new Promise((resolve, reject) => {
    const client = net.connect(socket);
    let response = '';
    client.on('data', chunk => { response += chunk; });
    client.on('end', () => resolve(response));
    client.on('error', reject);
    client.write(JSON.stringify({ method, params }) + '\n');
  });
  const reply = JSON.parse(raw);
  assert.equal(reply.ok, true, raw);
  return reply.value;
}

let pending;
try {
  const { generation } = await call('ping', {});
  const original = { sessionId: admitted, generation, requestId: '33333333-3333-4333-8333-333333333333' };
  const late = { sessionId: unseen, generation, requestId: '44444444-4444-4444-8444-444444444444' };
  pending = call('session-action', { ...original, action: 'resume' });
  await Promise.race([entered, pending.then(reply => assert.fail(`Resume settled before admission: ${JSON.stringify(reply)}`))]);
  assert.deepEqual(await call('session-resume-status', original), { ...original, status: 'pending' },
    'An in-flight resume must keep its pending receipt');

  assert.equal((await call('session-resume-status', late)).status, 'failed');
  assert.equal((await call('session-action', { ...late, action: 'resume' })).status, 'failed',
    'A status fence must refuse the delayed request for the unseen session');
  assert.deepEqual(resumes, [admitted]);

  release();
  assert.equal((await pending).status, 'completed');
  assert.deepEqual(await call('session-resume-status', original), { ...original, status: 'completed' });
  assert.deepEqual(resumes, [admitted]);
} finally {
  release();
  await pending?.catch(() => {});
  for (const dispose of cleanup.reverse()) await dispose();
}
"""


@unittest.skipUnless(shutil.which('node'), 'Node.js required')
class ResumeStatus(unittest.TestCase):
    def test_status_read_during_an_admitted_resume_preserves_its_receipt(self):
        with tempfile.TemporaryDirectory(prefix='ha-', dir=os.environ.get('ZERUS_TEST_SHORT_TMPDIR', '/tmp')) as root:
            result = subprocess.run(['node', '--input-type=module', '-', BRIDGE.as_uri(), str(Path(root) / 'host.sock')],
                                    input=RESUME_STATUS_HARNESS, text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)


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
const { callEnvironment, parseIdentity, identitySession, readIdentity, wrapSpawns, callTokens, CALL_TOKEN, HIDDEN_TOKEN } = await import(process.argv[2]);
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

    def test_repeated_install_keeps_both_callables_and_restores_the_prototype(self):
        self.run_js("""
const native = { spawn() { return this; }, spawnTerminal() { return this; } };
const service = Object.create(native);
const environment = () => null;
const installed = wrapSpawns(service, environment);
const spawn = service.spawn, terminal = service.spawnTerminal;
for (let i = 0; i < 1000; i++) {
  const repeated = wrapSpawns(service, environment);
  assert.equal(repeated, installed);
  assert.equal(service.spawn, spawn);
  assert.equal(service.spawnTerminal, terminal);
}
assert.equal(service.spawn({}), service);
assert.equal(service.spawnTerminal({}), service);
installed.restore();
assert.equal(Object.hasOwn(service, 'spawn'), false);
assert.equal(Object.hasOwn(service, 'spawnTerminal'), false);
assert.equal(service.spawn, native.spawn);
assert.equal(service.spawnTerminal, native.spawnTerminal);
""")

    def test_fresh_module_unwraps_another_installations_marked_methods(self):
        self.run_js("""
// Each module has a separate WeakMap. Only the marks connect them.
const other = await import(process.argv[2] + '?independent-installation');
const native = { spawn(spec) { return spec; }, spawnTerminal(spec) { return spec; } };
const service = Object.create(native);
const first = wrapSpawns(service, () => ({ HGS_SESSION: 'old', OLD_ONLY: 'stale' }));
const second = other.wrapSpawns(service, () => ({ HGS_SESSION: 'current' }));
first.restore();
for (const method of ['spawn', 'spawnTerminal']) {
  assert.deepEqual(service[method]({ env: { INPUT: 'kept' } }).env,
    { INPUT: 'kept', HGS_SESSION: 'current' }, method);
}
second.restore();
for (const method of ['spawn', 'spawnTerminal']) {
  assert.equal(Object.hasOwn(service, method), false, method);
  assert.equal(service[method], native[method], method);
}
""")

    def test_a_call_token_names_its_conversation_while_the_call_or_its_jobs_live(self):
        self.run_js("""
assert.equal(CALL_TOKEN, 'HGS_DSH_CALL_TOKEN');
const tokens = callTokens(3);
const call = tokens.open(conversation);
assert.match(call, /^[0-9a-f]{64}$/);
assert.notEqual(tokens.open(conversation), call, 'every call has its own token');
assert.equal(tokens.conversation(call), conversation);
// A job the call started keeps the token after the call returns, until it settles.
tokens.job(call, 'job-1');
tokens.close(call);
assert.equal(tokens.conversation(call), conversation);
tokens.settled('job-1');
assert.equal(tokens.conversation(call), undefined);
// Without a job, the token ends with its call; a settled job of another call is no release.
const plain = tokens.open(conversation);
tokens.settled('job-1');
assert.equal(tokens.conversation(plain), conversation);
tokens.close(plain);
assert.equal(tokens.conversation(plain), undefined);
for (const bad of [undefined, '', 'X'.repeat(64), call.toUpperCase(), call + '0', 7]) assert.equal(tokens.conversation(bad), undefined);
// Bounded: a full table recycles only the opener's own oldest token, so
// another conversation's live token stays (zerus-ppd26.13).
const first = tokens.open('session-00000000-0000-4000-8000-000000000001');
const own = [];
for (let n = 0; n < 3; n++) own.push(tokens.open(conversation));
assert.equal(tokens.conversation(first), 'session-00000000-0000-4000-8000-000000000001');
assert.equal(tokens.conversation(own[0]), undefined);
assert.equal(tokens.conversation(own[2]), conversation);
assert.equal(tokens.size, 3);
""")

    def test_one_conversation_never_pushes_out_anothers_tokens_and_live_ones_stay_hidden(self):
        # zerus-ppd26.5: a conversation evicts only its own oldest tokens, and
        # what the host serves never carries a live token.
        self.run_js("""
const tokens = callTokens(10, 3);
const other = 'session-00000000-0000-4000-8000-000000000001';
const theirs = tokens.open(other);
const mine = [];
for (let n = 0; n < 6; n++) mine.push(tokens.open(conversation));
assert.equal(tokens.conversation(theirs), other, 'another conversation keeps its token');
assert.deepEqual(mine.map(token => tokens.conversation(token) !== undefined), [false, false, false, true, true, true]);
assert.equal(tokens.size, 4);
const ended = mine[0], live = mine[5], unrelated = 'ab'.repeat(32);
const served = JSON.stringify({ output: `HGS_DSH_CALL_TOKEN=${live}\\nother ${unrelated} ended ${ended}` });
const hidden = tokens.redact(served);
assert.ok(!hidden.includes(live));
assert.ok(hidden.includes(`HGS_DSH_CALL_TOKEN=${HIDDEN_TOKEN}`));
assert.ok(hidden.includes(unrelated) && hidden.includes(ended), 'only live tokens are hidden');
tokens.close(live);
assert.ok(tokens.redact(served).includes(live), 'an ended token is no longer live');
""")

    def test_every_copy_of_a_live_token_is_hidden_whatever_precedes_it(self):
        # zerus-ppd26.13: each 64-character window of a hex run is tested, so a
        # token after a hex digit or a JSON-escaped control character is hidden,
        # and every copy in one reply (U1).
        self.run_js("""
const tokens = callTokens();
const live = tokens.open(conversation), other = tokens.open(conversation);
for (const text of [live, 'c' + live, 'deadbeef' + live, live + live, live + ' and again ' + live,
    String.fromCharCode(27) + live, String.fromCharCode(0) + live, String.fromCharCode(8) + live,
    String.fromCharCode(12) + live, 'x' + live + 'cafe', other + live]) {
  const served = tokens.redact(JSON.stringify({ output: text }));
  assert.ok(!served.includes(live), JSON.stringify(text.slice(0, 12)));
  assert.ok(!served.includes(other));
  assert.ok(served.includes(HIDDEN_TOKEN));
}
const twice = tokens.redact(JSON.stringify({ output: live + ' then ' + live }));
assert.equal(twice.split(HIDDEN_TOKEN).length - 1, 2, 'both copies');
// Hex around a token stays as it was.
assert.ok(tokens.redact('deadbeef' + live + 'cafe').startsWith('deadbeef' + HIDDEN_TOKEN + 'cafe'));
const unrelated = 'a'.repeat(200);
assert.equal(tokens.redact(unrelated), unrelated);
""")

    def test_a_multi_megabyte_hex_run_is_redacted_without_overflow(self):
        # zerus-ppd26.14: a single 4-8 MiB hex run made the old
        # /[0-9a-f]{64,}/g matcher throw RangeError inside String.replace;
        # the bounded matcher keeps the reply readable and the token hidden.
        self.run_js("""
const tokens = callTokens();
const live = tokens.open(conversation);
const huge = 'a'.repeat(5 * 1024 * 1024);
const served = tokens.redact(JSON.stringify({ output: huge + live + huge }));
assert.ok(!served.includes(live), 'the live token in the middle is hidden');
assert.ok(served.includes(HIDDEN_TOKEN));
assert.ok(served.startsWith('{"output":"aaaa'));
""")

    def test_a_full_table_never_takes_another_conversations_live_token(self):
        # zerus-ppd26.13: a conversation makes room only from its own tokens; a
        # newcomer facing a full table gets none and its call stays unproven.
        self.run_js("""
const tokens = callTokens(4, 3);
const a = 'session-00000000-0000-4000-8000-00000000000a', b = 'session-00000000-0000-4000-8000-00000000000b';
const c = 'session-00000000-0000-4000-8000-00000000000c';
const mine = [tokens.open(a), tokens.open(a), tokens.open(a)], theirs = tokens.open(b);
assert.equal(tokens.size, 4);
assert.equal(tokens.open(c), undefined, 'a newcomer takes no token of another conversation');
assert.ok(mine.every(token => tokens.conversation(token) === a) && tokens.conversation(theirs) === b);
const next = tokens.open(a);
assert.equal(tokens.conversation(mine[0]), undefined, 'a recycles its own oldest');
assert.equal(tokens.conversation(next), a);
assert.equal(tokens.conversation(theirs), b);
const again = tokens.open(b);
assert.equal(tokens.conversation(theirs), undefined);
assert.equal(tokens.conversation(again), b);
assert.ok(mine.slice(1).every(token => tokens.conversation(token) === a));
""")


# The real bridge's apply() over a stand-in host whose prompt inserts scripted
# messages, as the harness does, before it resolves (zerus-hfw2s.2).
WITNESS_HARNESS = r"""
import net from 'node:net';
const [bridgeUrl, socket, scenario] = process.argv.slice(2);
const { apply } = await import(bridgeUrl);
const listeners = {}, effects = [];
const session = id => {
  const log = [];
  return { id, header: { origin: 'user' }, log, get seq() { return log.length; },
    snapshotEvents(from = 0, to = log.length) { return log.slice(from, to); } };
};
const agent = id => ({ id, session: session(id), status: 'idle', inbox: { nextStep: [], nextTurn: [] },
  model: { current: () => ({ provider: 'fixture', model: 'm' }), select() {} } });
const agents = new Map(['S1', 'S2'].map(id => [id, agent(id)]));
const insert = (target, message) => {
  const log = target.session.log;
  log.push({ seq: log.length, type: 'agent/inbox/spliced',
    data: { target: 'next-step', start: target.inbox.nextStep.length, inserted: [message] } });
  target.inbox.nextStep.push(message);
  for (const listener of listeners['agent/inbox/inserted'] ?? []) listener({ agent: target, message });
};
const prompt = async p => {
  const message = id => ({ id, role: 'user', source: { kind: 'user', rpcId: p.requestId }, content: p.content });
  if (scenario === 'refused') throw new Error('Scripted refusal before any insert');
  // Another session's message with the same request ID comes first.
  if (scenario === 'other-session-first') insert(agents.get('S2'), message('foreign'));
  insert(agents.get(p.sessionId), message('m1'));
  // A later message with the same request ID on the same session.
  if (scenario === 'later-insert') insert(agents.get(p.sessionId), message('m2'));
  if (scenario === 'inserted-then-throws') throw new Error('Scripted failure after the insert');
  return { accepted: true };
};
const ctx = new Proxy({
  sessionController: { prompt, list: async () => ({ items: [] }), projections: async () => ({ values: {} }),
    agents: { composeAgent: async () => ({}), presetForObservation() {}, serializeImageAdmission() {} } },
  agents: { get: id => agents.get(id), roots: () => [...agents.values()] },
  goals: { get: () => null },
  get: name => (name === 'sessions' ? { flush: async () => true } : undefined),
  on: (name, listener) => { (listeners[name] ??= []).push(listener); return () => {}; },
  effect: run => { effects.push(run()); },
}, { get: (target, key) => (key in target ? target[key] : undefined) });
apply(ctx, { socket });
await Promise.all(effects);
const call = (method, params = {}) => new Promise((resolve, reject) => {
  const client = net.connect(socket);
  let data = '';
  client.on('data', chunk => { data += chunk; });
  client.on('end', () => resolve(JSON.parse(data)));
  client.on('error', reject);
  client.write(JSON.stringify({ method, params }) + '\n');
});
const { value: { generation } } = await call('ping');
const sent = await call('send', { sessionId: 'S1', requestId: 'R', content: [{ type: 'text', text: 'X' }], mode: 'steer' });
const witness = sessionId => call('input-witness', { sessionId, requestId: 'R', generation, waitMs: 0 });
process.stdout.write(JSON.stringify({ sent, s1: await witness('S1'), s2: await witness('S2') }));
process.exit(0);
"""


@unittest.skipUnless(shutil.which('node'), 'Node.js required')
class InputWitnessBinding(unittest.TestCase):
    """zerus-hfw2s.2: a request is bound to the FIRST message the harness inserts
    for it on the sending session, never to another session's or a later one."""

    def answers(self, scenario):
        with tempfile.TemporaryDirectory(prefix='hgs-dsh-witness-', dir=os.environ.get('ZERUS_TEST_SHORT_TMPDIR', '/tmp')) as root:
            result = subprocess.run(['node', '--input-type=module', '-', BRIDGE.as_uri(), str(Path(root) / 'host.sock'), scenario],
                                    input=WITNESS_HARNESS, text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def bound(self, answer):
        self.assertTrue(answer['ok'], answer)
        value = answer['value']
        return value['state'], value.get('messageId'), value.get('spliceSeq')

    def test_a_later_insert_with_the_same_request_id_never_rebinds(self):
        answers = self.answers('later-insert')
        self.assertEqual(self.bound(answers['s1']), ('tracked', 'm1', 0))

    def test_another_sessions_insert_never_binds(self):
        answers = self.answers('other-session-first')
        self.assertEqual(self.bound(answers['s1']), ('tracked', 'm1', 0))
        self.assertEqual(self.bound(answers['s2']), ('unseen', None, None))

    def test_only_a_request_with_no_insert_is_refused(self):
        refused = self.answers('refused')
        self.assertFalse(refused['sent']['ok'])
        self.assertEqual(refused['s1']['value']['state'], 'refused')
        inserted = self.answers('inserted-then-throws')
        self.assertFalse(inserted['sent']['ok'])
        self.assertEqual(self.bound(inserted['s1']), ('tracked', 'm1', 0))




@unittest.skipUnless(shutil.which('node'), 'Node.js required')
class LifecycleEnds(unittest.TestCase):
    """zerus-63rru.102: the bridge's end-record map stays at 4096 (the oldest
    dropped), and only the host's own disposal reads as ended."""
    def run_js(self, body):
        script = """
import assert from 'node:assert/strict';
const { apply } = await import(process.argv[2]);
const control = () => ({
  composeAgent: async () => ({ setup() {}, agentPreset: 'standard' }),
  presetForObservation() {}, serializeImageAdmission() {},
  selectionFor() { throw Error('read installed a selection'); },
  selectModel() { throw Error('profile default changed'); },
});
""" + body
        result = subprocess.run(['node', '--input-type=module', '-', BRIDGE.as_uri()],
                                input=script, text=True, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_disposals_beyond_4096_drop_the_oldest_and_only_disposals_read_ended(self):
        import tempfile as tf
        with tf.TemporaryDirectory() as d:
            sock = os.path.join(d, 'bridge.sock')
            body = f"""
const sock = {sock!r};
const handlers = new Map();
const ctx = {{
  sessionController: {{ agents: control() }},
  get() {{ return undefined; }},
  on(event, fn) {{
    const list = handlers.get(event) ?? handlers.set(event, []).get(event);
    list.push(fn);
  }},
  effect(fn) {{ Promise.resolve(fn()).catch(() => {{}}); }},
  agents: new Map(),
}};
apply(ctx, {{ socket: sock }});
const fire = (event, payload) => handlers.get(event)?.forEach(fn => fn(payload));
for (let n = 0; n < 4097; n++) fire('agent/disposed', {{ agent: {{ session: {{ id: `session-${{n}}` }} }} }});
const net = await import('node:net');
async function ask(sessionId) {{
  for (let attempt = 0; attempt < 50; attempt++) {{
    try {{
      return await new Promise((resolve, reject) => {{
        const s = net.connect(sock);
        s.on('error', reject);
        s.on('data', chunk => {{ resolve(JSON.parse(chunk.toString())); s.destroy(); }});
        s.write(JSON.stringify({{ method: 'session-lifecycle', params: {{ sessionId }} }}) + '\\n');
      }});
    }} catch (error) {{ await new Promise(r => setTimeout(r, 50)); }}
  }}
  throw new Error('bridge socket never answered');
}}
const oldest = await ask('session-0');
assert.equal(oldest.value.state, 'unknown');
// The bound keeps exactly 4096: the second-oldest is still there, so the
// drop is a bound, not a larger eviction.
const second = await ask('session-1');
assert.equal(second.value.state, 'ended');
const newest = await ask('session-4096');
assert.equal(newest.value.state, 'ended');
assert.equal(newest.value.end.source, 'agent/disposed');
process.exit(0);
"""
            self.run_js(body)

    def test_a_disposed_session_reads_ended_through_the_lifecycle(self):
        import tempfile as tf
        with tf.TemporaryDirectory() as d:
            sock = os.path.join(d, 'bridge.sock')
            body = f"""
const sock = {sock!r};
const handlers = new Map();
const ctx = {{
  sessionController: {{ agents: control() }},
  get() {{ return undefined; }},
  on(event, fn) {{
    const list = handlers.get(event) ?? handlers.set(event, []).get(event);
    list.push(fn);
  }},
  effect(fn) {{ Promise.resolve(fn()).catch(() => {{}}); }},
  agents: new Map(),
}};
apply(ctx, {{ socket: sock }});
const fire = (event, payload) => handlers.get(event)?.forEach(fn => fn(payload));
fire('agent/disposed', {{ agent: {{ session: {{ id: 'session-x' }} }} }});
const net = await import('node:net');
const answer = await new Promise((resolve, reject) => {{
  const s = net.connect(sock);
  s.on('error', reject);
  s.on('data', chunk => {{ resolve(JSON.parse(chunk.toString())); s.destroy(); }});
  s.write(JSON.stringify({{ method: 'session-lifecycle', params: {{ sessionId: 'session-x' }} }}) + '\\n');
}});
assert.equal(answer.value.state, 'ended');
assert.equal(answer.value.end.source, 'agent/disposed');
process.exit(0);
"""
            self.run_js(body)


if __name__ == '__main__':
    unittest.main()

