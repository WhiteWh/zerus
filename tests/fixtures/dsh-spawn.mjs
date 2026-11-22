// Private test tool using the official resident subprocess service.
import { renameSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const rawService = Symbol.for('cordis.original');
const spawnOriginal = Symbol.for('hgs.dsh.spawn.original');
const installations = new WeakMap();
const previousServices = new WeakMap();
const spawnMethods = ['spawn', 'spawnTerminal'];
let nextInstallation = 0;

function nativeMethods(service) {
  return spawnMethods.map(method => !Object.hasOwn(service, method)
    && service[method] === Object.getPrototypeOf(service)[method]);
}

function installationNumber(service) {
  const spawn = service.spawn;
  if (!installations.has(spawn)) installations.set(spawn, ++nextInstallation);
  return installations.get(spawn);
}

function spec(cwd) {
  return { argv: ['/usr/bin/true'], cwd, graceMs: 1000,
    stdio: { stdin: 'ignore', stdout: 'ignore', stderr: 'ignore' } };
}

function spawnUnderProbe(ctx, cwd) {
  return ctx.get('subprocess').spawn(spec(cwd));
}

export async function probeSpawns(ctx, cwd, action, evidenceDir) {
  const service = ctx.get('subprocess')[rawService];
  if (action === 'swap') {
    // Use the real Cordis registry and a second native runtime. The isolated
    // registration lets us construct it without disposing the original.
    const replacement = new service.constructor(ctx.isolate('subprocess'));
    service.ctx.set('subprocess', replacement);
    if (ctx.get('subprocess')[rawService] !== replacement) {
      service.ctx.set('subprocess', service);
      throw new Error('The private Cordis subprocess service did not change');
    }
    previousServices.set(replacement, service);
    return { replacementNative: nativeMethods(replacement) };
  }
  if (action === 'cleanup' || action === 'cleanup-swap') {
    const previous = action === 'cleanup-swap' && previousServices.get(service);
    if (action === 'cleanup-swap' && !previous) throw new Error('The private subprocess swap is missing');
    const previousRestored = previous && nativeMethods(previous);
    const bridge = [...ctx.get('loader').entries()].find(({ options }) =>
      options?.id === 'hgs-native-dsh' || options?.id?.endsWith(':hgs-native-dsh'));
    if (!bridge) throw new Error('Private HGS bridge entry is missing');
    const wrapped = spawnMethods.map(method => Object.hasOwn(service, method));
    // Run outside the tool call. The bridge cleanup also closes its own agents.
    setImmediate(async () => {
      let result;
      try {
        await bridge.update({ disabled: true });
        const restored = nativeMethods(service);
        if (previous) {
          previous.ctx.set('subprocess', previous);
          previousServices.delete(service);
        }
        await bridge.update({ disabled: null });
        result = previous ? { wrapped, restored, previousRestored } : { wrapped, restored };
      } catch (error) {
        result = { error: String(error) };
      }
      const file = join(evidenceDir, previous ? 'spawn-swap-cleanup.json' : 'spawn-cleanup.json');
      writeFileSync(file + '.new', JSON.stringify(result));
      renameSync(file + '.new', file);
    });
    return { scheduled: true };
  }
  if (action === 'damage') {
    // Install a recognizable chain of stale HGS layers, then observe the
    // real failure before returning control to the native tool hook.
    let wrapped = Object.getPrototypeOf(service).spawn;
    for (let i = 0; i < 12000; i++) {
      const next = wrapped;
      wrapped = function staleSpawn(...args) { return next.apply(this, args); };
      Object.defineProperty(wrapped, spawnOriginal, { value: next });
    }
    service.spawn = wrapped;
    try {
      await spawnUnderProbe(ctx, cwd).done;
      return { error: 'The stale wrapper chain unexpectedly spawned a process' };
    } catch (error) {
      return { error: String(error) };
    }
  }

  const count = action === 'bulk' ? 500 : 1;
  const wrapperMarked = spawnMethods.map(method => typeof service[method]?.[spawnOriginal] === 'function');
  const installation = installationNumber(service);
  const depths = [];
  const select = service.selectContainmentMode;
  const originalDescriptor = Object.getOwnPropertyDescriptor(service, 'selectContainmentMode');
  let completed = 0;
  for (let i = 0; i < count; i++) {
    const stackLimit = Error.stackTraceLimit;
    let handle;
    // Observe exactly where the native implementation admits each spawn.
    // Count only the frames up to this dedicated synchronous call site.
    service.selectContainmentMode = function (...args) {
      const depth = new Error().stack.split('\n').findIndex(frame => frame.includes('at spawnUnderProbe '));
      depths.push(depth);
      return select.apply(this, args);
    };
    try {
      Error.stackTraceLimit = Infinity;
      handle = spawnUnderProbe(ctx, cwd);
    } catch (error) {
      return { completed, error: String(error) };
    } finally {
      Error.stackTraceLimit = stackLimit;
      if (originalDescriptor) Object.defineProperty(service, 'selectContainmentMode', originalDescriptor);
      else delete service.selectContainmentMode;
    }
    const result = await handle.done;
    if (result.exitCode !== 0) return { completed, error: `Spawn exit ${result.exitCode}` };
    completed++;
  }
  return { completed, installation, wrapperMarked, minDepth: Math.min(...depths), maxDepth: Math.max(...depths) };
}
