// HGS adapter for reviewed official @deepseek-ai/dsh 0.2.1 rc.2/rc.5 and
// 0.2.2-rc.1 hosts. Its ping version identifies the adapter, not the harness.
// Loaded through its supported Cordis --patch extension mechanism. No fork of
// the engine, credential copying, HTTP authentication bypass or PTY injection.
// One method wrapper adds explicit environment entries, the subprocess
// service's documented caller entries, to processes a tool call starts.
import net from 'node:net';
import fs from 'node:fs/promises';
import { closeSync, constants, fstatSync, openSync, readSync } from 'node:fs';
import { join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { AsyncLocalStorage } from 'node:async_hooks';
import { isDeepStrictEqual } from 'node:util';

export const name = 'hgs-native-dsh';

// zerus-ppd26. One host serves every DeepSeek session of its agent home, so
// its own environment names no session: hgs starts it without the markers of
// whichever session launched it. Every tool call's processes instead carry
// HGS_AGENT=dsh and exactly the calling session's own markers, or none of
// them; an explicit undefined entry is the subprocess service's tombstone
// that removes an inherited value. Never a partial or inherited set.
const SESSION_MARKERS = Object.freeze(['HGS_SESSION', 'HGS_RUN_ID', 'HGS_SESSION_ID', 'HGS_ACCOUNT_ID']);
const IDENTITY_BYTES = 4096;
const NATIVE_SESSION = /^session-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function callEnvironment(identity) {
  const env = { HGS_AGENT: 'dsh' };
  for (const key of SESSION_MARKERS) env[key] = undefined;
  if (identity) Object.assign(env, { HGS_SESSION: identity.session, HGS_RUN_ID: identity.run,
    HGS_SESSION_ID: identity.session_id, HGS_ACCOUNT_ID: identity.account });
  return Object.freeze(env);
}

const plainMarker = value => typeof value === 'string' && value.length > 0 && value.length <= 2048
  && [...value].every(c => { const n = c.codePointAt(0); return n > 0x1f && (n < 0x7f || n > 0x9f); });

// The file hgs writes for one native conversation (state/dsh/shell_identity.rs).
export function parseIdentity(text, sessionId) {
  let value;
  try { value = JSON.parse(text); } catch { return undefined; }
  if (value?.version !== 1 || value.conversation !== sessionId) return undefined;
  const { session, run, session_id, account } = value;
  return [session, run, session_id, account].every(plainMarker) ? { session, run, session_id, account } : undefined;
}

// A child agent's processes carry its top-level session's markers, as an
// in-process subagent of another agent shares that agent's environment.
export function identitySession(agent) {
  const header = agent?.session?.header;
  const id = header?.origin === 'subagent' ? header.parentSession : (agent?.session?.id ?? header?.id);
  return typeof id === 'string' && NATIVE_SESSION.test(id) ? id : undefined;
}

export function readIdentity(dir, sessionId) {
  if (typeof dir !== 'string' || !dir || !sessionId) return undefined;
  let fd;
  try {
    fd = openSync(join(dir, sessionId + '.json'), constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    const meta = fstatSync(fd);
    if (!meta.isFile() || meta.size > IDENTITY_BYTES) return undefined;
    const buffer = Buffer.alloc(IDENTITY_BYTES + 1);
    const read = readSync(fd, buffer, 0, buffer.length, 0);
    return read > IDENTITY_BYTES ? undefined : parseIdentity(buffer.subarray(0, read).toString('utf8'), sessionId);
  } catch { return undefined; } finally { if (fd !== undefined) closeSync(fd); }
}

// Adds the current call's entries to each process the service starts while
// that call runs; later caller entries never override them.
export function wrapSpawns(service, current) {
  const wrapped = [];
  for (const method of ['spawn', 'spawnTerminal']) {
    const original = service?.[method];
    if (typeof original !== 'function') continue;
    const wrapper = function (spec, ...rest) {
      const env = current();
      return original.call(this, env && spec && typeof spec === 'object' ? { ...spec, env: { ...spec.env, ...env } } : spec, ...rest);
    };
    service[method] = wrapper;
    wrapped.push([method, original, wrapper]);
  }
  return {
    installed: wrapped.some(([method]) => method === 'spawn'),
    current: () => wrapped.every(([method, , wrapper]) => service[method] === wrapper),
    restore() { for (const [method, original, wrapper] of wrapped) if (service[method] === wrapper) service[method] = original; },
  };
}
export const inject = ['sessionController', 'agents', 'goals', 'llm', 'agentDefaultModel', 'sessionQuery', 'userQuestions', 'workspaceRegistry', 'subagents', 'permissionPresets', 'approval', 'commands'];

// 0.2.2 moved selection from the controller's WeakMap to the Agent's native
// model object. Both reads are side-effect-free; never use selectionFor or
// public selectModel (which also saves the profile default) as a fallback.
export function modelSelectionAdapter(control) {
  const incompatible = () => new Error('HGS: incompatible official dsh session controller');
  if (typeof control?.composeAgent !== 'function' || typeof control?.presetForObservation !== 'function'
      || typeof control?.serializeImageAdmission !== 'function') throw incompatible();
  const legacy = control.selections instanceof WeakMap && typeof control.selectForNextRequest === 'function';
  // A partial old interface is not the reviewed new interface.
  if (!legacy && (control.selections !== undefined || control.selectForNextRequest !== undefined)) throw incompatible();
  const modelOf = agent => {
    const model = agent?.model;
    if (typeof model?.current !== 'function' || typeof model?.select !== 'function') throw incompatible();
    return model;
  };
  return {
    legacy,
    async compose(preset) {
      const composition = await control.composeAgent(preset);
      if (legacy) return composition;
      if (typeof composition?.setup !== 'function') throw incompatible();
      // The native factory runs setup before publishing/starting the Agent.
      return { ...composition, setup(agentCtx, agent) {
        modelOf(agent);
        return composition.setup(agentCtx, agent);
      } };
    },
    has(agent) { return legacy ? control.selections.has(agent) : Boolean(modelOf(agent)); },
    current(agent, projections) {
      if (!legacy) {
        const current = modelOf(agent).current();
        return current === undefined ? null : { ...current };
      }
      const installed = control.selections.get(agent);
      if (installed) return { ...installed.current };
      const stored = projections?.modelSelection?.next;
      if (stored) return stored;
      const { provider, model, reasoningEffort } = agent.options ?? {};
      return provider && model ? { provider, model, ...(reasoningEffort === undefined ? {} : { reasoningEffort }) } : null;
    },
    // Called only inside the SAME native serializeImageAdmission chain used
    // by image prompts. No private queue, global default write or option edit.
    select(agent, selection) {
      if (legacy) control.selectForNextRequest(agent, selection);
      else modelOf(agent).select(selection);
    },
  };
}

function prefixHash(events, count) {
  if (!Array.isArray(events) || !Number.isSafeInteger(count) || count < 1 || count > events.length) return null;
  const hash = createHash('sha256');
  for (let i = 0; i < count; i++) {
    if (events[i]?.seq !== i) return null;
    hash.update(JSON.stringify(events[i])); hash.update('\n');
  }
  return hash.digest('hex');
}

// 0.2.2 intentionally omits parentSession on ordinary forks. Pin an exact
// completed source cut BEFORE the call, then verify its immutable inherited
// bytes and native seed boundary. Missing parent metadata alone proves nothing.
export function completedForkCut(source) {
  const atSeq = source.events.findLastIndex(event => event.type === 'turn/end');
  const sha256 = prefixHash(source.events, atSeq + 1);
  if (sha256 === null) throw new Error('Wait for a completed reply before forking');
  return { atSeq, sha256 };
}

export function matchesForkCut(created, cut) {
  const count = cut.atSeq + 1, marker = created.events[count];
  return created.header.isSeeded === true && created.header.parentSession === undefined
    && created.inheritedEventCount === count && marker?.seq === count
    && marker.type === 'session/end-seed' && marker.data?.inherited === true
    && prefixHash(created.events, count) === cut.sha256;
}

// A 0.2.2 task belongs to the Host; only its actual destination binds it to
// this session. Being a creator/editor in involvedSessionIds is insufficient.
export function scheduleSession(entry) {
  if (Object.hasOwn(entry, 'target'))
    return entry.target?.kind === 'session' ? entry.target.sessionId : undefined;
  return entry.sessionId;
}

function scheduleRecord(entry) {
  const record = { ...entry };
  for (const key of ['sessionId', 'status', 'lastDelivery']) delete record[key];
  return record;
}

// Preserve the session-scoped wire contract over the reviewed Host-wide v2
// service. Its version compare happens INSIDE the native edit queue, so a
// retarget after our read refuses instead of mutating another session's task.
export async function manageSchedule(schedule, p) {
  const exact = { sessionId: p.sessionId, id: p.id };
  const history = { id: p.id, limit: p.limit, ...(p.before === undefined ? {} : { before: p.before }) };
  if (typeof schedule.update === 'function' && typeof schedule.delete === 'function') {
    if (p.action === 'history') return schedule.history({ ...exact, ...history });
    if (p.action === 'delete') return schedule.delete(exact);
    return schedule.update({ ...exact, expected: p.expected,
      ...Object.fromEntries(['title', 'prompt', 'change'].filter(key => p[key] !== undefined).map(key => [key, p[key]])) });
  }
  if (schedule.update !== undefined || schedule.delete !== undefined
      || typeof schedule.view !== 'function' || typeof schedule.edit !== 'function')
    throw new Error('HGS: incompatible official dsh schedule service');
  const current = await schedule.view({ id: p.id });
  if (scheduleSession(current) !== p.sessionId)
    return { id: p.id, code: 'schedule_not_found', ...(p.action === 'delete' ? { deleted: false } : p.action === 'update' ? { updated: false } : {}) };
  if (p.action === 'history') {
    const result = await schedule.history(history);
    const after = await schedule.view({ id: p.id });
    if (scheduleSession(after) !== p.sessionId || after.updatedAt !== current.updatedAt)
      return { id: p.id, code: 'schedule_not_found' };
    // A Host task can have been retargeted earlier too. Its opaque native
    // cursor is preserved, but another destination's deliveries stay private.
    return Array.isArray(result.records) ? { ...result, records: result.records.filter(row => row.targetSessionId === p.sessionId) } : result;
  }
  if (p.action === 'update' && !isDeepStrictEqual(p.expected, scheduleRecord(current)))
    return { id: p.id, updated: false, code: 'schedule_conflict' };
  const request = { id: p.id, action: p.action, expectedUpdatedAt: current.updatedAt ?? null };
  if (p.action === 'update') {
    request.patch = Object.fromEntries(['title', 'prompt'].filter(key => p[key] !== undefined).map(key => [key, p[key]]));
    if (p.change !== undefined) {
      if (!['after', 'at', 'every', 'daily', 'weekly', 'cron', 'rrule'].includes(p.change?.kind))
        throw new Error('Unsupported native schedule timing');
      request.patch.timing = p.change;
    }
  }
  const result = await schedule.edit(request);
  if (p.action === 'delete') return result;
  return result.code ? { ...result, updated: false } : { id: p.id, updated: true, record: scheduleRecord(result) };
}

export function apply(ctx, config) {
  const api = ctx.sessionController;
  const control = api.agents;
  const models = modelSelectionAdapter(control);
  const owned = new Map(), opening = new Map(), questions = new Map();
  const generation = randomUUID();
  const shellContext = new AsyncLocalStorage(), shellCalls = new Map(), jobCalls = new Map();
  const callContext = new AsyncLocalStorage();
  const identities = typeof config.shellIdentities === 'string' && config.shellIdentities ? config.shellIdentities : null;
  let spawns = null, spawnService = null;
  // A reloaded subprocess service is wrapped again before the next call.
  function callEnvironmentInstalled() {
    const service = ctx.get('subprocess');
    if (spawns && service === spawnService && spawns.current()) return true;
    spawns?.restore();
    spawns = wrapSpawns(service, () => callContext.getStore());
    spawnService = spawns.installed ? service : null;
    return spawns.installed;
  }
  callEnvironmentInstalled();
  ctx.on('dispose', () => spawns?.restore());
  const jobRegistry = ctx.get('jobs');
  if (jobRegistry) ctx.on('dispose',jobRegistry.events.subscribe({owners:'scope'},event => {
    const call = shellContext.getStore();
    if (event.type === 'registered' && event.job.kind === 'bash' && call?.owner === event.job.owner) {
      call.jobId=event.job.id;jobCalls.set(event.job.id,call);
    }
    if (event.type === 'removed') jobCalls.delete(event.job.id);
  }));
  // Every tool call, whatever the tool or agent: its processes carry
  // HGS_AGENT=dsh and the calling top-level session's markers or none.
  ctx.on('tools/execute', (exec, next) => {
    callEnvironmentInstalled();
    const env = callEnvironment(readIdentity(identities, identitySession(exec.agent)));
    return callContext.run(env, () => observeShell(exec, next));
  });
  function observeShell(exec, next) {
    if (exec.name !== 'bash' || !exec.agent?.session?.id) return next();
    const call={callId:exec.callId,owner:exec.agent.session.id,command:String(exec.arguments?.command ?? '').slice(0,16000),
      cwd:exec.arguments?.workdir?.slice(0,4096),startedAt:Date.now(),status:'running'};
    const key=JSON.stringify([call.owner,call.callId]);shellCalls.set(key,call);
    while(shellCalls.size>512)shellCalls.delete(shellCalls.keys().next().value);
    return shellContext.run(call,async()=>{
      try {
        const result=await next();
        call.status=result.isError?'failed':result.value?.kind==='background'||result.value?.kind==='promoted'?'running':'completed';
        if(call.status!=='running')call.finishedAt=Date.now();
        call.exitCode=result.value?.exitCode;
        return result;
      } catch(error){call.status='failed';call.finishedAt=Date.now();throw error;}
    });
  }
  const recovery = new Map(), recoveryReceipts = new Map();
  const compactions = new Map(), compactReceipts = new Set();
  const forkReceipts = new Map(), forking = new Map();
  // Optional official Schedule bundle. Absent means unknown, never "no tasks".
  const scheduleService = () => ctx.get('schedule');
  const recurringKinds = new Set(['every', 'daily', 'weekly', 'cron', 'rrule']);
  async function scheduleSummaries() {
    const schedule = scheduleService();
    if (typeof schedule?.catalog !== 'function') return null;
    try {
      const summaries = new Map();
      for (const entry of await schedule.catalog()) {
        const sessionId = scheduleSession(entry);
        if (entry.status !== 'active' || typeof sessionId !== 'string' || !sessionId) continue;
        const summary = summaries.get(sessionId) ?? { count: 0, recurring: 0, nextAt: null };
        const at = Date.parse(entry.scheduledAt) / 1000;
        summary.count += 1;
        if (recurringKinds.has(entry.kind)) summary.recurring += 1;
        if (Number.isFinite(at) && (summary.nextAt === null || at < summary.nextAt)) summary.nextAt = at;
        summaries.set(sessionId, summary);
      }
      return summaries;
    } catch { return null; }
  }
  const compactionPublic = sessionId => compactions.get(sessionId)?.public ?? null;
  const recoveryPublic = sessionId => recovery.get(sessionId)?.public ?? null;
  async function recoveryEnabled() {
    if (!config.recoveryPolicy) return false;
    try {
      const policy = JSON.parse(await fs.readFile(config.recoveryPolicy, 'utf8'));
      const heartbeat = JSON.parse(await fs.readFile(config.recoveryPolicy.replace(/policy\.json$/, 'heartbeat.json'), 'utf8'));
      return policy.enabled === true && heartbeat.enabled === true && Date.now() / 1000 - heartbeat.at < 30;
    } catch { return false; }
  }
  // Native retry policy goes first. HGS only owns the final failed model step.
  ctx.on('agent/request-error', async (request, next) => {
    const native = await next();
    const sessionId = request.agent?.session?.id;
    if (native || !owned.has(sessionId) || closing || !await recoveryEnabled()) return native;
    return new Promise(resolve => {
      const id = randomUUID();
      const finish = result => {
        if (recovery.get(sessionId)?.public.id === id) recovery.delete(sessionId);
        clearInterval(timer); request.signal?.removeEventListener('abort', abort); resolve(result);
      };
      const abort = () => finish(undefined);
      const timer = setInterval(async () => { if (!await recoveryEnabled()) finish(undefined); }, 3000);
      timer.unref();
      const failure = request.failure;
      const retryAfter = Number(failure?.providerRetryAfterMs ?? 0);
      recovery.set(sessionId, { finish, public: { id, at: Date.now() / 1000,
        turn: request.turn, step: request.step,
        detail: `${failure?.status ?? ''} ${failure?.code ?? ''} ${failure?.message ?? 'Provider request failed'}`.trim(),
        retryNotBefore: Number.isFinite(retryAfter) && retryAfter > 0 ? (Date.now() + retryAfter) / 1000 : 0 } });
      request.signal?.addEventListener('abort', abort, { once: true });
      if (request.signal?.aborted) abort();
    });
  }, { prepend: true });
  let closing = false;
  // Read only the native account's public projection, never its credentials.
  const accountStatus = async () => (await ctx.get('deepseekAccount')?.getState())?.status ?? 'unavailable';
  // deepseek-official uses API keys; a signed-out account must not block it.
  const needsSignIn = (selection, status) => selection?.provider === 'deepseek-account' && status === 'signed-out';
  async function publishWorkspace(sessionId) {
    const observation = await ctx.sessionQuery.observeSession(sessionId);
    try {
      if (observation.header.origin === 'subagent') throw new Error('Use the parent workspace for a subagent');
      const workspace = await ctx.workspaceRegistry.create(observation.header.cwd);
      await workspace.attachSession(sessionId);
      return { workspaceId: workspace.id, path: workspace.path, sessionIds: [...workspace.sessionIds] };
    } finally { observation[Symbol.dispose](); }
  }
  function applyPermissions(agent, mode) {
    if (mode === 'bypass') ctx.permissionPresets.apply(agent.session, 'danger-full-access',
      policy => ctx.approval.setPolicy(agent, policy));
  }
  function checkPermissions(mode) {
    if (mode !== undefined && !['provider', 'bypass'].includes(mode)) throw new Error('Unknown account permission mode');
  }
  // The native default route, effort included, as the official session
  // controller's agentOptions(). A selection the session already recorded
  // still wins on resume; this is only the route of a session without one.
  function defaultAgentOptions() {
    const { provider, model, reasoningEffort } = ctx.agentDefaultModel.currentSelection();
    return { provider, model, ...(reasoningEffort === undefined ? {} : { reasoningEffort }) };
  }
  async function resume(sessionId, permissionMode) {
    checkPermissions(permissionMode);
    const live = ctx.agents.get(sessionId);
    if (live) return live;
    if (opening.has(sessionId)) return opening.get(sessionId);
    const work = (async () => {
      const observation = await ctx.sessionQuery.observeSession(sessionId);
      try {
        if (observation.header.origin === 'subagent') throw new Error('Use the parent to manage a subagent');
        const composition = await models.compose(control.presetForObservation(observation));
        const handle = await ctx.agents.resume({ resumeSessionId: sessionId, agentOptions: defaultAgentOptions(), setup: composition.setup });
        applyPermissions(handle.agent, permissionMode);
        owned.set(sessionId, handle);
        await publishWorkspace(sessionId);
        return handle.agent;
      } finally { observation[Symbol.dispose](); }
    })().finally(() => opening.delete(sessionId));
    opening.set(sessionId, work);
    return work;
  }
  // The 0.2.1 controller installs a selection while composing a top-level Agent or
  // changing its model. selectionFor would also install one on a read of any
  // other Agent, such as a child: prompt listeners that take over its request
  // route (the profile default before its first request), and an effect Cordis
  // rejects on an unloading scope. Reads never install one; an Agent without
  // one reports its stored selection, or before its first request its own route.
  const selectionOf = (agent, projections) => models.current(agent, projections);
  // A child lives in its parent's Agent scope and belongs to subagent routing:
  // upstream neither prompts nor reconfigures one directly, offers it no
  // commands and interrupts it only through its parent's address. HGS messages
  // it through send-child. Send, settings, compact and interrupt refuse any
  // other than a top-level Agent before they change it.
  const topLevel = agent => {
    if (agent && (agent.session.header.origin === 'subagent' || !ctx.agents.roots().includes(agent)))
      throw new Error('Use the parent to manage a subagent');
    return agent;
  };
  const publicQuestions = sessionId => [...questions.values()].filter(q => q.sessionId === sessionId)
    .map(({ id, items, approval }) => ({ id, questions: items, approval: Boolean(approval) }));
  // A live request is the only authority for answering. Disconnecting HGS does
  // not answer, dismiss, or approve it. Native cancellation releases the wait.
  ctx.on('user-questions/request', (request, next) => {
    const sessionId = request.agent?.session?.id;
    if (!owned.has(sessionId) || closing) return next();
    return new Promise((resolve, reject) => {
      const id = request.wait?.callId ?? randomUUID();
      const abort = () => { questions.delete(id); reject(request.signal.reason ?? new Error('Question cancelled')); };
      if (request.signal?.aborted) { abort(); return; }
      const settle = answer => { questions.delete(id); request.signal?.removeEventListener('abort', abort); resolve(answer); };
      questions.set(id, { id, sessionId, items: request.questions, settle });
      request.signal?.addEventListener('abort', abort, { once: true });
      // Timed questions must claim the native wait, not invent a separate timer.
      if (request.wait?.timed) {
        (async () => { for await (const _ of ctx.userQuestions.attachWait(request.agent, id, request.signal)) {} })()
          .catch(() => {});
      }
    });
  }, { prepend: true });
  ctx.on('approval/request', (request, next) => {
    const sessionId = request.agent?.session?.id;
    if (!owned.has(sessionId) || closing) return next();
    return new Promise(resolve => {
      const id = randomUUID();
      const abort = () => { questions.delete(id); resolve('cancelled'); };
      if (request.signal?.aborted) { abort(); return; }
      const settle = answer => {
        questions.delete(id); request.signal?.removeEventListener('abort', abort);
        resolve(answer.answers[0].selected[0] === 'Allow once' ? 'allowed-once' : 'rejected');
      };
      questions.set(id, { id, sessionId, approval: true, settle, items: [{ id: 'decision',
        header: 'Approval required', question: `Allow ${request.toolName}?`, detail: request.reason ?? request.displayReason?.en ?? '',
        options: [{ label: 'Allow once' }, { label: 'Reject' }] }] });
      request.signal?.addEventListener('abort', abort, { once: true });
    });
  }, { prepend: true });

  async function childPage(address) {
    const abort = new AbortController();
    const stream = api.follow({ address, maxMessages: 150 }, abort.signal);
    try {
      const first = await stream.next();
      if (first.value?.type !== 'snapshot') throw new Error('Native child history is unavailable');
      return { baseline: first.value.projections, page: { records: first.value.records, hasMore: first.value.hasMore } };
    } finally { abort.abort(); await stream.return(); }
  }
  // Native authorization reports a child without its own descriptor as corrupt.
  // The engine now records it when it creates a child. A one-shot child stored
  // by 0.2.0-rc.2 recorded it in its first step, after its parent cataloged it
  // and after that step's durability checkpoint; one cancelled before that step
  // stays without it. Such a child has no public history.
  async function childHistory(address) {
    try { return await childPage(address); } catch (error) {
      if (error?.code !== 'subagent/catalog-diagnostic') throw error;
      const state = await childEstablishment(address).catch(() => 'unknown');
      if (state === 'unstarted') return { baseline: { asOfSeq: -1, values: {} }, page: { records: [], hasMore: false }, unstarted: true };
      if (state !== 'recorded') throw error;
    }
    // A descriptor recorded since the first read passes native validation;
    // a corrupt one fails it again.
    return childPage(address);
  }
  async function childEstablishment({ parentSessionId, childSessionId }) {
    const observation = await ctx.sessionQuery.observeSession(childSessionId, { projectionMode: 'none' });
    try {
      const { header, inheritedEventCount } = observation;
      if (header.origin !== 'subagent' || header.parentSession !== parentSessionId) return 'unrelated';
      const own = observation.events.slice(inheritedEventCount);
      if (own.some(event => event.type === 'subagent/descriptor')) return 'recorded';
      // Without a step, a live child has yet to record its descriptor and a
      // stored one never will; a child that ran steps without one is corrupt.
      return own.some(event => event.type === 'step/start') ? 'missing' : 'unstarted';
    } finally { observation[Symbol.dispose](); }
  }
  async function inspect(request) {
    const { sessionId, agentId } = request;
    let baseline = await api.projections({ sessionId }, new AbortController().signal);
    if (!baseline) throw new Error('Native session no longer exists');
    const child = agentId ? baseline.values.subagentCatalog?.find(child => child.id === agentId) : null;
    const jobOwners = agentId ? [agentId] : [sessionId, ...(baseline.values.subagentCatalog ?? []).map(child => child.id)];
    let address = { kind: 'session', sessionId };
    let page, unstarted = false;
    if (agentId) {
        // Native page authorization verifies the direct parent/child relation.
        address = { kind: 'subagent', parentSessionId: sessionId, childSessionId: agentId, mode: 'unknown' };
        ({ baseline, page, unstarted = false } = await childHistory(address));
    }
    page ??= await api.page({ address, throughSeq: baseline.asOfSeq, maxMessages: 150 }, new AbortController().signal);
    const lifecycleRows = agentId ? null : await api.list({}, new AbortController().signal);
    const lifecycleIdle = lifecycleRows?.items?.find(row => row.sessionId === sessionId)?.running === false;
    const parentLive = ctx.agents.get(sessionId);
    const live = agentId ? ctx.agents.get(agentId) : parentLive;
    const selection = live ? selectionOf(live, baseline.values) : null;
    // Read the goal before awaiting: a disposing Agent may leave the live
    // registry meanwhile, and the goal registry rejects it once it has.
    const goal = live ? ctx.goals.get(live) ?? null : baseline.values.goal ?? null;
    const status = await accountStatus();
    const jobs = ctx.get('jobs');
    return { generation, baseline, page,
      lifecycleActions: agentId ? [] : ["rename", ...(!parentLive ? ["resume", "forget"] : owned.get(sessionId)?.agent === parentLive ? [...(lifecycleIdle ? ["pause"] : []), "forget"] : [])],
      jobs: jobs && request.includeProcesses !== false ? jobOwners.flatMap(owner => jobs.list(owner).filter(job => job.kind === 'bash' && job.owner === owner)
        .map(job => ({ ...job, callId:jobCalls.get(job.id)?.callId, controllable: owned.has(sessionId) && Boolean(ctx.agents.get(owner)) }))) : undefined,
      shellCalls:request.includeProcesses !== false ? [...shellCalls.values()].filter(call=>jobOwners.includes(call.owner)) : undefined,
      childMode: child?.mode, childSendSupported: !unstarted && child?.mode === 'continuable' && Boolean(parentLive), goal,
      pendingQuestions: publicQuestions(sessionId), selection, accountStatus: status, authRequired: needsSignIn(selection, status),
      live: Boolean(live), owned: owned.has(sessionId), nativeRecovery: recoveryPublic(sessionId), interruptSupported: typeof api.cancel === 'function',
      compactSupported: Boolean(live && ctx.commands.find(live,'compact')?.definitionId === '@deepseek-ai/dsh-command-compact'), compactRequest: agentId ? null : compactionPublic(sessionId),
      // Native fork copies the latest completed-turn prefix. An older page
      // can hold it; the native operation remains the authoritative check.
      forkSupported: !agentId && typeof api.fork === 'function',
      forkReady: !agentId && (page.hasMore === true || page.records.some(row => row.event?.type === 'turn/end')) };
  }
  // The official fork remote creates a new session identity from the exact
  // latest completed-turn prefix and leaves its live Agent with the native
  // Session Controller. Receipts make an uncertain reply retryable.
  async function fork(p) {
    if (p.generation !== generation) throw new Error('DeepSeek host changed; refresh before forking');
    if (typeof p.requestId !== 'string' || !/^[0-9a-f-]{36}$/.test(p.requestId)) throw new Error('Invalid fork request ID');
    const previous = forkReceipts.get(p.requestId) ?? await forking.get(p.requestId);
    if (previous) {
      if (previous.sourceSessionId !== p.sessionId) throw new Error('Fork request ID was already used for another session');
      return previous;
    }
    if (typeof api.fork !== 'function') throw new Error('Native fork is unavailable');
    const work = (async () => {
      const source = await ctx.sessionQuery.observeSession(p.sessionId, { projectionMode: 'none' });
      let cwd, cut;
      try {
        if (source.header.origin === 'subagent') throw new Error('Fork the parent session; a child belongs to its parent');
        if (typeof p.cwd !== 'string' || source.header.cwd !== p.cwd) throw new Error('Native session belongs to another folder');
        cwd = source.header.cwd;
        if (!models.legacy) cut = completedForkCut(source);
      } finally { source[Symbol.dispose](); }
      const { sessionId } = await api.fork({ sessionId: p.sessionId, ...(cut ? { atSeq: cut.atSeq } : {}) });
      const created = await ctx.sessionQuery.observeSession(sessionId, { projectionMode: 'none' });
      try {
        if (sessionId === p.sessionId || created.header.id !== sessionId
            || (cut ? !matchesForkCut(created, cut) : created.header.parentSession !== p.sessionId) || created.header.cwd !== cwd
            || created.header.origin === 'subagent')
          throw new Error(`Native fork ${sessionId} does not match its source; inspect Native UI`);
      } finally { created[Symbol.dispose](); }
      const receipt = { requestId: p.requestId, sourceSessionId: p.sessionId, sessionId, cwd, generation,
        live: Boolean(ctx.agents.get(sessionId)), owned: owned.has(sessionId) };
      if (typeof p.title === 'string' && p.title) {
        try { await api.rename({ sessionId, title: p.title }); }
        catch (error) { receipt.titleError = String(error?.message ?? error); }
      }
      forkReceipts.set(p.requestId, receipt);
      if (forkReceipts.size > 1000) forkReceipts.delete(forkReceipts.keys().next().value);
      return receipt;
    })().finally(() => forking.delete(p.requestId));
    forking.set(p.requestId, work);
    return work;
  }
  // Text parts and identities only: reasoning and stream records stay private.
  const publicSources = new Set(['user', 'user-question-reply']);
  function publicMessage(event) {
    const assistant = event.type === 'assistant/message';
    const message = assistant ? event.data?.message : event.data;
    const content = typeof message?.content === 'string' ? message.content
      : (Array.isArray(message?.content) ? message.content : []).filter(part => part?.type === 'text' && typeof part.text === 'string')
        .map(part => ({ type: 'text', text: part.text }));
    const body = { id: message?.id, role: message?.role, content };
    return { type: event.type, seq: event.seq, time: event.time,
      data: assistant ? { message: body } : { ...body, source: { kind: event.data?.source?.kind } } };
  }
  // Exact bound sessions only, through the store's literal case-insensitive
  // event scan. It works without the optional full-text index and never
  // resumes an Agent. Each hit is re-read so callers see the complete event,
  // including its source, and only public message parts.
  async function searchHistory(p) {
    const query = typeof p.query === 'string' ? p.query.trim() : '';
    if (!query || query.length > 500 || query.includes('\0')) throw new Error('Invalid search query');
    if (!Array.isArray(p.sessionIds) || p.sessionIds.length > 500 || p.sessionIds.some(id => typeof id !== 'string'))
      throw new Error('Invalid search sessions');
    const perSession = Math.min(Math.max(Number.isSafeInteger(p.perSession) ? p.perSession : 20, 1), 50);
    // Interactive search waits for every host; later sessions report skipped.
    const deadline = Date.now() + 5000;
    const sessions = [];
    for (const sessionId of new Set(p.sessionIds)) {
      if (Date.now() > deadline) { sessions.push({ sessionId, status: 'skipped' }); continue; }
      try {
        const matches = await ctx.sessionQuery.filterEvents(sessionId, [
          { kind: 'type', values: ['user/message', 'assistant/message'] }, { kind: 'text', text: query }]);
        // Newest first. Injected context is a user message too, so reads are
        // bounded separately from the person's and the agent's own messages.
        const events = [];
        let read = 0;
        for (const match of [...matches].reverse()) {
          if (events.length === perSession || read === perSession * 2 || Date.now() > deadline) break;
          read += 1;
          const { target } = await ctx.sessionQuery.readEvent({ sessionId, seq: match.seq, before: 0, after: 0 });
          if (target?.seq === match.seq && (target.type === 'assistant/message' || publicSources.has(target.data?.source?.kind)))
            events.push(publicMessage(target));
        }
        sessions.push({ sessionId, status: 'ok', events, truncated: read < matches.length });
      } catch (error) {
        sessions.push({ sessionId, status: error?.code === 'SESSION_QUERY_SESSION_NOT_FOUND' ? 'missing' : 'unavailable' });
      }
    }
    return { generation, sessions };
  }
  // Official Schedule tasks bound to one exact session. Reads, deletion and
  // edits never activate it; update is the native compare-and-set.
  async function schedules(p) {
    const schedule = scheduleService();
    if (typeof p.sessionId !== 'string' || !p.sessionId) throw new Error('Choose a native session');
    if (!schedule) return { available: false, generation };
    if (p.action === 'list') {
      const items = (await schedule.catalog()).filter(entry => scheduleSession(entry) === p.sessionId);
      return { available: true, generation, items };
    }
    if (p.generation !== generation) throw new Error('DeepSeek host changed; refresh schedules');
    if (typeof p.id !== 'string' || !p.id) throw new Error('Choose a scheduled task');
    if (!['history', 'delete', 'update'].includes(p.action)) throw new Error('Unsupported schedule action');
    if (p.action === 'update' && (!p.expected || typeof p.expected !== 'object')) throw new Error('Expected task record is required');
    return { available: true, generation, result: await manageSchedule(schedule, p) };
  }
  async function execute(request) {
    if (closing) throw new Error('Native host is stopping');
    const p = request.params ?? {};
    switch (request.method) {
      case 'ping': return { protocol: 1, version: '0.2.1-rc.2', generation, accountPermissions: true, sessionActions: true,
        shellIdentity: callEnvironmentInstalled(),
        nativeOperations: ['fork', 'history-search', 'schedules'], schedule: Boolean(scheduleService()) };
      case 'list': {
        const list = await api.list({}, new AbortController().signal);
        const status = await accountStatus();
        const scheduled = await scheduleSummaries();
        return { ...list, generation, items: list.items.map(row => {
          const agent = ctx.agents.get(row.sessionId);
          const selection = agent ? selectionOf(agent, row.projections?.values) : null;
          return { ...row, selection, accountStatus: status, authRequired: needsSignIn(selection, status),
            goal: agent ? ctx.goals.get(agent) ?? null : row.projections?.values.goal ?? null,
            pendingQuestions: publicQuestions(row.sessionId), nativeRecovery: recoveryPublic(row.sessionId),
            ...(scheduled ? { schedules: scheduled.get(row.sessionId) ?? { count: 0, recurring: 0, nextAt: null } } : {}) };
        }) };
      }
      case 'inspect': return inspect(p);
      case 'fork': return fork(p);
      case 'search-history': return searchHistory(p);
      case 'schedules': return schedules(p);
      case 'job-output':
      case 'job-stop': {
        if (p.generation !== generation) throw new Error('DeepSeek host changed; refresh Processes');
        const fresh = await inspect({sessionId:p.sessionId});
        const job = fresh.jobs?.find(job => job.id === p.jobId && job.owner === p.owner);
        if (!job) throw new Error('This process no longer belongs to the selected session');
        const jobs = ctx.get('jobs');
        if (request.method === 'job-stop') {
          if (!job.controllable) throw new Error('Native process control is unavailable');
          return {status:jobs.kill(job.id,job.owner,'Stopped from Zerus Processes'),generation};
        }
        const from = Math.max(0,job.output.total-32768);
        const read = jobs.readAt(job.id,from,job.owner);
        const bytes = Buffer.from(read.chunks.map(chunk => chunk.text).join(''),'utf8');
        return {output:bytes.subarray(Math.max(0,bytes.length-32768)).toString('utf8'),
          truncated:from>0 || read.lossy || bytes.length>32768, status:job.status, generation};
      }
      case 'compact': {
        if (p.generation !== generation) throw new Error('DeepSeek host changed');
        const agent = topLevel(ctx.agents.get(p.sessionId));
        if (!agent || ctx.commands.find(agent,'compact')?.definitionId !== '@deepseek-ai/dsh-command-compact') throw new Error('Native compaction is unavailable');
        if (compactReceipts.has(p.requestId) || compactionPublic(p.sessionId)?.status === 'compacting') throw new Error('Compaction was already requested');
        const abort = new AbortController();
        const operation = ctx.commands.execute(agent, '/compact', [], abort.signal);
        const state = { public: { request_id: p.requestId, status: 'compacting', at: Date.now() / 1000 }, abort };
        compactions.set(p.sessionId, state); compactReceipts.add(p.requestId);
        operation.then(async result => {
          if (state.public.status !== 'compacting') return;
          state.revision = (await api.projections({sessionId:p.sessionId}, new AbortController().signal)).asOfSeq;
          state.public.status = result?.result.kind !== 'success' ? 'failed' : result.result.sourceEventSeq !== undefined ? 'completed' : 'unchanged';
          state.public.error = result?.result.kind === 'error' ? result.result.text : undefined;
          state.public.completed_at = Date.now() / 1000;
        }).catch(error => { state.public.status = 'failed'; state.public.error = String(error?.message ?? error); });
        return { status: 'submitted' };
      }
      case 'interrupt': {
        if (p.generation !== generation) throw new Error('DeepSeek host changed');
        topLevel(ctx.agents.get(p.sessionId));
        const fresh = await inspect({ sessionId: p.sessionId });
        const started = fresh.page.records.findLast(row => row.event?.type === 'turn/start')?.event.time / 1000;
        // The registry may change while the inspection awaits.
        const live = topLevel(ctx.agents.get(p.sessionId));
        if (!live || !(started > 0) || started !== p.turnStarted) throw new Error('DeepSeek turn changed; nothing was stopped');
        ctx.goals.disarm(live);
        const compaction = compactions.get(p.sessionId);
        if (compaction?.public.status === 'compacting') {
          compaction.public.status = 'cancelled'; compaction.abort.abort();
        }
        recovery.get(p.sessionId)?.finish(undefined);
        return api.cancel({ sessionId: p.sessionId });
      }
      case 'cancel-recovery': {
        const pending = recovery.get(p.sessionId);
        if (pending?.public.id === p.id) pending.finish(undefined);
        return { cancelled: true };
      }
      case 'recover': {
        if (p.generation !== generation) throw new Error('DeepSeek host changed');
        const previous = recoveryReceipts.get(p.requestId);
        if (previous) {
          if (previous.sessionId !== p.sessionId || previous.id !== p.id) throw new Error('Recovery request ID reused');
          return previous;
        }
        const pending = recovery.get(p.sessionId);
        if (!pending || pending.public.id !== p.id || !await recoveryEnabled()) throw new Error('Failed request is no longer pending');
        if (Date.now() / 1000 < pending.public.retryNotBefore) throw new Error('Provider retry deadline has not passed');
        const receipt = { requestId: p.requestId, sessionId: p.sessionId, id: p.id, generation, status: 'confirmed' };
        recoveryReceipts.set(p.requestId, receipt);
        if (recoveryReceipts.size > 1000) recoveryReceipts.delete(recoveryReceipts.keys().next().value);
        pending.finish({ kind: 'retry' });
        return receipt;
      }
      case 'workspace': return publishWorkspace(p.sessionId);
      case 'catalog': return api.modelCatalog();
      case 'create': {
        checkPermissions(p.permissionMode);
        if (typeof p.cwd !== 'string' || !p.cwd.startsWith('/') || !(await fs.stat(p.cwd)).isDirectory()) throw new Error('Choose an existing absolute folder');
        const sessionId = p.sessionId;
        if (typeof sessionId !== 'string' || !/^session-[0-9a-f-]{36}$/.test(sessionId)) throw new Error('Invalid native session ID');
        if (ctx.agents.get(sessionId)) {
          const observation=await ctx.sessionQuery.observeSession(sessionId);
          try {if (observation.header.cwd !== p.cwd) throw new Error('Native session belongs to another folder');}
          finally {observation[Symbol.dispose]();}
          await publishWorkspace(sessionId);
          return {sessionId,generation};
        }
        try {
          const observation = await ctx.sessionQuery.observeSession(sessionId);
          try {
            if (observation.header.cwd !== p.cwd) throw new Error('Native session belongs to another folder');
          } finally { observation[Symbol.dispose](); }
          await resume(sessionId, p.permissionMode); return { sessionId, generation };
        } catch (error) {
          if (error?.code !== 'SESSION_QUERY_SESSION_NOT_FOUND') throw error;
        }
        const composition = await models.compose('standard');
        const handle = await ctx.agents.create({ sessionId, agentOptions: defaultAgentOptions(),
          meta: { cwd: p.cwd, agentPreset: composition.agentPreset }, setup: composition.setup });
        applyPermissions(handle.agent, p.permissionMode);
        owned.set(sessionId, handle);
        await publishWorkspace(sessionId);
        if (p.title) await api.rename({ sessionId, title: p.title });
        return { sessionId, generation };
      }
      case 'session-action': {
        let changed = false;
        const answer = (status, error) => ({ sessionId:p.sessionId, action:p.action, generation, status, ...(error ? {error} : {}) });
        try {
          const listed = p.action === 'pause' ? await api.list({},new AbortController().signal) : null;
          if (p.generation !== generation) throw new Error('DeepSeek host changed; refresh before this action');
          if (!['pause','resume','rename','forget'].includes(p.action) || typeof p.sessionId !== 'string') throw new Error('Unsupported scoped lifecycle action');
          const live = ctx.agents.get(p.sessionId);
          const handle = owned.get(p.sessionId);
          if (p.action === 'pause' && listed?.items?.find(row => row.sessionId === p.sessionId)?.running === true) throw new Error('Wait for the native turn before pausing');
          if (live && p.action !== 'rename' && handle?.agent !== live) throw new Error('This agent is owned by the native UI; manage it there');
          if (p.action === 'resume') {
            if (live || opening.has(p.sessionId)) throw new Error('This session is already running or opening');
            checkPermissions(p.permissionMode);
            changed = true;
            await resume(p.sessionId,p.permissionMode);
          } else if (p.action === 'rename') {
            if (typeof p.title !== 'string' || !p.title || p.title.length > 512) throw new Error('Invalid scoped title');
            changed = true; await api.rename({sessionId:p.sessionId,title:p.title});
          } else if (live) {
            changed = true; ctx.goals.disarm(live);
            await handle.dispose();
            if (owned.get(p.sessionId) === handle) owned.delete(p.sessionId);
          }
          return answer('completed');
        } catch (error) { return answer(changed ? 'uncertain' : 'failed',String(error?.message ?? error)); }
      }
      case 'resume': await resume(p.sessionId, p.permissionMode); return { generation };
      case 'pause': {
        // Closing the owned handle drains exactly this agent and its children;
        // other sessions and the shared native host remain alive.
        const live = ctx.agents.get(p.sessionId);
        if (!live) return { stopped: true };
        const handle = owned.get(p.sessionId);
        if (!handle) throw new Error('This agent is owned by the native UI; stop it there');
        ctx.goals.disarm(live);
        await handle.dispose(); owned.delete(p.sessionId);
        return { stopped: true };
      }
      case 'rename': return api.rename(p);
      case 'send-child': {
        const parent = ctx.agents.get(p.parentSessionId);
        if (!parent) throw new Error('Resume the parent session before messaging this agent');
        return ctx.subagents.prompt(p, new AbortController().signal);
      }
      case 'send': {
        topLevel(ctx.agents.get(p.sessionId));
        const compact = compactionPublic(p.sessionId);
        if (p.expectedCompactionId && (compact?.request_id !== p.expectedCompactionId || compact.status !== 'completed'))
          throw new Error('Compaction is not confirmed; message was not sent');
        if (p.expectedCompactionId && compactions.get(p.sessionId).revision !== (await api.projections({sessionId:p.sessionId}, new AbortController().signal)).asOfSeq)
          throw new Error('Conversation changed after compaction; message was not sent');
        // A message consumes the continuation even if another client sent it.
        if (compact) compact.status = 'cancelled';
        recovery.get(p.sessionId)?.finish(undefined);
        const agent = ctx.agents.get(p.sessionId);
        if (!agent) throw new Error('Resume this session before sending a message');
        // Read as list and inspect do. A top-level Agent upstream composed has an
        // installed selection; another one, such as a webhook root, gets none here.
        const selection = selectionOf(agent, models.has(agent) ? undefined
          : (await api.projections({ sessionId: p.sessionId }, new AbortController().signal))?.values);
        if (needsSignIn(selection, await accountStatus()))
          throw new Error('Sign in to DeepSeek in the native UI before sending a message');
        return api.prompt(p, new AbortController().signal);
      }
      case 'settings': {
        // A launch applying account defaults binds the exact host generation
        // that created the session. A paused session resumes under the
        // account permission mode HGS passes, as a scoped resume does.
        if (p.generation !== undefined && p.generation !== generation) throw new Error('DeepSeek host changed; refresh before changing settings');
        checkPermissions(p.permissionMode);
        const agent = topLevel(await resume(p.sessionId, p.permissionMode));
        const catalog = await api.modelCatalog();
        const model = catalog.groups.find(g => g.id === p.provider)?.models.find(m => m.id === p.model);
        if (!model) throw new Error('This model is unavailable for the native account');
        if (p.reasoningEffort !== undefined && !model.reasoning?.efforts?.some(e => e.id === p.reasoningEffort)) throw new Error('Unsupported reasoning effort');
        return control.serializeImageAdmission(agent, async () => {
          const resolved = await ctx.llm.resolveCallConfig({ provider: p.provider, model: p.model,
            ...(p.reasoningEffort === undefined ? {} : { reasoningEffort: p.reasoningEffort }) });
          const selection = { provider: resolved.provider, model: resolved.model,
            ...(resolved.reasoningEffort === undefined ? {} : { reasoningEffort: resolved.reasoningEffort }) };
          models.select(agent, selection);
          return { selected: selection, scope: 'session' };
        });
      }
      case 'answer': {
        const q = questions.get(p.id);
        if (p.generation !== generation || !q || q.sessionId !== p.sessionId) throw new Error('This question is no longer pending');
        if (!Array.isArray(p.answers) || p.answers.length !== q.items.length) throw new Error('Answer every question');
        const seen = new Set();
        for (const answer of p.answers) {
          const item = q.items.find(i => i.id === answer.id);
          if (!item || seen.has(answer.id) || !Array.isArray(answer.selected)
              || answer.selected.some(label => !item.options?.some(o => o.label === label))
              || (!item.multiSelect && answer.selected.length > 1)
              || (answer.selected.length === 0 && !answer.custom?.trim())
              || (q.approval && answer.selected.length !== 1)
              || (answer.custom !== undefined && typeof answer.custom !== 'string')) throw new Error('Invalid question answer');
          seen.add(answer.id);
        }
        q.settle({ answers: p.answers }); return { accepted: true };
      }
      default: throw new Error('Unsupported HGS native request');
    }
  }

  // A private Unix socket is a local plugin transport, never an HTTP endpoint.
  // The native web application's own authentication remains untouched.
  const server = net.createServer(socket => {
    let bytes = 0, chunks = [], handled = false;
    socket.setTimeout(45000, () => socket.destroy());
    socket.on('error', () => {});
    socket.on('data', chunk => {
      if (handled) return;
      bytes += chunk.length;
      if (bytes > 30 * 1024 * 1024) { socket.destroy(); return; }
      chunks.push(chunk);
      if (!chunk.includes(10)) return;
      handled = true;
      (async () => {
        try {
          const request = JSON.parse(Buffer.concat(chunks).toString('utf8'));
          socket.end(JSON.stringify({ ok: true, value: await execute(request) }) + '\n');
        } catch (error) {
          socket.end(JSON.stringify({ ok: false, error: String(error?.message ?? error), code: error?.code }) + '\n');
        }
      })();
    });
  });
  ctx.effect(async () => {
    await new Promise((resolve, reject) => {
      server.once('error', reject); server.listen(config.socket, resolve);
    });
    await fs.chmod(config.socket, 0o600);
    return async () => {
      closing = true; server.close();
      for (const handle of owned.values()) await handle.dispose();
      owned.clear(); await fs.unlink(config.socket).catch(() => {});
    };
  }, 'hgs.native-dsh');
}
