import { createSignal } from "solid-js";
import { tr } from "../i18n";
import { trText, type Text } from "../text";
import {
  modelServerDelete, modelServerDown, modelServerLogs, modelServerSave, modelServerSession, modelServerSessions, modelServerStatus,
  modelServersList, modelServerTest, modelServerTestLogs, modelServerUp, modelServerUpResult, modelServerVariant,
  type ModelServer, type ModelServerForm, type ModelServerLogs, type ModelServerStatus, type ModelServerTest,
  type ModelServerUpPoll, type ModelServerUpStart,
} from "../bridge";

// Task 67: Qt's ModelServers::Controller. The servers of this computer and
// their status. While the tab shows, each server is read every 60 s, at most
// two reads at a time and never two of one server; Up, Down and Logs run
// once when asked. client/core reads every reply (model_servers.rs).

export interface Port {
  list(): Promise<ModelServer[]>;
  status(server: string): Promise<ModelServerStatus>;
  up(server: string, model: string): Promise<ModelServerUpStart>;
  upResult(server: string, model: string, id: string, expired: boolean): Promise<ModelServerUpPoll>;
  sessions(server: string): Promise<Record<string, string[]>>;
  down(server: string, model: string): Promise<Text | null>;
  /** model-catalog/2: a confirmed variant change; a running service restarts. */
  variant?(server: string, model: string, variant: string, restart: boolean): Promise<Text | null>;
  logs(server: string, model: string): Promise<ModelServerLogs>;
  save(form: ModelServerForm): Promise<ModelServer[]>;
  remove(name: string): Promise<ModelServer[]>;
  test(form: ModelServerForm, what: "api" | "status"): Promise<ModelServerTest>;
  testLogs(form: ModelServerForm, model: string): Promise<ModelServerTest>;
  session(agent: string, folder: string, tag: string, server: string, model: string): Promise<void>;
}

export interface Clock {
  now(): number;
  every(ms: number, run: () => void): () => void;
  after(ms: number, run: () => void): void;
}

const realClock: Clock = {
  now: () => Date.now(),
  every: (ms, run) => { const id = setInterval(run, ms); return () => clearInterval(id); },
  after: (ms, run) => { setTimeout(run, ms); },
};

export interface StatusEntry { known: boolean; reading: boolean; status?: ModelServerStatus; readAt: number }
export interface Notice { text: string; error: boolean }

export const MAX_READS = 2;
export const INTERVAL = 60_000;
export const UP_POLL = 5_000;
/** Owner spec 2026-11-14: a catalog server is read every 8 s while the tab shows. */
export const CATALOG_INTERVAL = 8_000;
/** After an action, a catalog server is read until no service is starting or stopping. */
export const SETTLE_POLL = 5_000;
export const SETTLE_LIMIT = 20 * 60_000;

/** A core Text error, an Error or anything else, as words. */
export function words(error: unknown): string {
  if (error && typeof error === "object" && "source" in error) return trText(error as Text);
  return error instanceof Error ? error.message : String(error);
}

export function createModelServers(port: Port, clock: Clock = realClock) {
  const [servers, setServers] = createSignal<ModelServer[]>([]);
  const [listed, setListed] = createSignal(false);
  const [statuses, setStatuses] = createSignal<Record<string, StatusEntry>>({});
  const [upping, setUpping] = createSignal<ReadonlySet<string>>(new Set());
  const [notice, setNoticeSignal] = createSignal<Notice>();
  let listing = false, active = false, stop: (() => void) | undefined, stopCatalog: (() => void) | undefined;
  const queue: string[] = [];
  const reading = new Set<string>();

  const server = (name: string) => servers().find((s) => s.name === name);
  const entry = (name: string): StatusEntry => statuses()[name] ?? { known: false, reading: false, readAt: 0 };
  const setEntry = (name: string, value: StatusEntry) => setStatuses({ ...statuses(), [name]: value });
  const say = (text: string, error = false) => setNoticeSignal(text ? { text, error } : undefined);
  const upKey = (s: string, m: string) => `${s}\n${m}`;

  function adopt(list: ModelServer[]) {
    setServers(list); setListed(true);
    const names = new Set(list.map((s) => s.name));
    setStatuses(Object.fromEntries(Object.entries(statuses()).filter(([name]) => names.has(name))));
    // A page that shows reads a server it has not read yet, once.
    if (active) for (const s of list) if (!entry(s.name).known) refreshStatus(s.name);
  }

  function pump() {
    while (reading.size < MAX_READS && queue.length) {
      const name = queue.shift()!;
      const target = server(name);
      if (!target) continue;
      reading.add(name);
      port.status(name)
        .then((status) => ({ known: true, reading: false, status, readAt: clock.now() }))
        .catch((error): StatusEntry => ({ known: true, reading: false, readAt: clock.now(), status: failedStatus(words(error)) }))
        .then((value) => { reading.delete(name); if (server(name)) setEntry(name, value); pump(); });
    }
  }

  function refreshStatus(name: string) {
    if (!server(name) || reading.has(name) || queue.includes(name)) return;
    queue.push(name);
    setEntry(name, { ...entry(name), reading: true });
    pump();
  }
  const refreshAll = () => { for (const s of servers()) refreshStatus(s.name); };
  const catalogServer = (s: ModelServer) => s.protocol === "model-catalog/2";
  const refreshCatalogs = () => { for (const s of servers()) if (catalogServer(s)) refreshStatus(s.name); };
  /** After an action: read again until no service is starting or stopping, within SETTLE_LIMIT. */
  function settle(target: string, until = clock.now() + SETTLE_LIMIT) {
    clock.after(SETTLE_POLL, () => {
      refreshStatus(target);
      const moving = entry(target).status?.models.some((m) => m.state === "loading" || m.state === "stopping");
      if ((moving || entry(target).reading) && clock.now() < until) settle(target, until);
    });
  }

  function pollUp(target: string, model: string, id: string, until: number) {
    clock.after(UP_POLL, () => {
      void port.upResult(target, model, id, clock.now() >= until).then((poll) => {
        if (poll.running) { pollUp(target, model, id, until); return; }
        finishUp(target, model, poll.outcome);
      }, (error) => finishUp(target, model, { text: { source: "%1", args: [{ kind: "verbatim", value: words(error) }] }, error: true }));
    });
  }
  function finishUp(target: string, model: string, outcome: { text: Text; error: boolean } | null) {
    const next = new Set(upping()); next.delete(upKey(target, model)); setUpping(next);
    if (outcome) say(trText(outcome.text), outcome.error);
    refreshStatus(target);
  }

  return {
    servers, listed, statuses, notice, server, entry,
    available: () => servers().length > 0,
    upRunning: (s: string, m: string) => upping().has(upKey(s, m)),
    clearNotice: () => say(""),
    say,
    /** `model-server list --json`; an hgs without model servers lists none. */
    async refreshList() {
      if (listing) return;
      listing = true;
      try { adopt(await port.list()); }
      catch { adopt([]); }
      finally { listing = false; }
    },
    refreshStatus, refreshAll,
    /** The tab shows: background reads run every minute; hidden, they stop. */
    setActive(on: boolean) {
      if (active === on) return;
      active = on;
      stop?.(); stop = undefined;
      stopCatalog?.(); stopCatalog = undefined;
      if (!on) return;
      stop = clock.every(INTERVAL, refreshAll);
      stopCatalog = clock.every(CATALOG_INTERVAL, refreshCatalogs);
      // The first look reads every server once; later ones follow the timer.
      for (const s of servers()) if (!entry(s.name).known) refreshStatus(s.name);
    },
    /** Up detaches; its run's own outcome shows when it ends. */
    up(target: string, model: string) {
      const key = upKey(target, model);
      if (upping().has(key)) return;
      setUpping(new Set([...upping(), key]));
      say(tr("Up %1 on %2…", model, target));
      void port.up(target, model).then((start) => {
        if (!start.started) { finishUp(target, model, start.outcome); return; }
        say(tr("Up %1 on %2 is running once; its result shows here when it ends.", model, target));
        refreshStatus(target);
        pollUp(target, model, start.id, clock.now() + start.limit_ms);
      }, (error) => finishUp(target, model, { text: { source: "Up %1 on %2 failed: %3", args: [{ kind: "verbatim", value: model }, { kind: "verbatim", value: target }, { kind: "verbatim", value: words(error) }] }, error: true }));
    },
    sessions: (target: string) => port.sessions(target),
    /** One down per model, one after the other, after the owner confirmed. */
    async down(target: string, models: string[]) {
      const failures: string[] = [];
      for (const model of models) {
        say(tr("Down %1 on %2…", model, target));
        try { const why = await port.down(target, model); if (why) failures.push(`${model}: ${trText(why)}`); }
        catch (error) { failures.push(`${model}: ${words(error)}`); }
        refreshStatus(target);
      }
      if (failures.length) say(tr("Down on %1 failed: %2", target, failures.join("; ")), true);
      else say(models.length === 1 ? tr("Down ran once on %1.", target) : tr("Down ran once for %1 models on %2.", models.length, target));
    },
    /** model-catalog/2: one confirmed variant change, then reads until it settles. */
    async variant(target: string, model: string, variant: string, restart: boolean) {
      if (!port.variant) return;
      say(restart ? tr("Restarting %1 on %2 as %3…", model, target, variant) : tr("Selecting %3 for %1 on %2…", model, target, variant));
      try {
        const why = await port.variant(target, model, variant, restart);
        if (why) say(tr("The variant of %1 on %2 did not change: %3", model, target, trText(why)), true);
        else say(restart ? tr("%1 on %2 restarts as %3; its state shows here.", model, target, variant) : tr("%1 on %2 now uses %3.", model, target, variant));
      } catch (error) { say(tr("The variant of %1 on %2 did not change: %3", model, target, words(error)), true); }
      refreshStatus(target);
      settle(target);
    },
    logs: (target: string, model: string) => port.logs(target, model),
    async save(form: ModelServerForm) { const list = await port.save(form); setServers(list); setListed(true); return list; },
    async remove(name: string) { const list = await port.remove(name); adopt(list); return list; },
    test: (form: ModelServerForm, what: "api" | "status") => port.test(form, what),
    testLogs: (form: ModelServerForm, model: string) => port.testLogs(form, model),
    async startSession(agent: string, folder: string, tag: string, target: string, model: string) {
      say(tr("Starting %1 on %2 with %3…", model, target, agent));
      try { await port.session(agent, folder, tag, target, model); }
      catch (error) { say(words(error), true); }
    },
  };
}
export type ModelServers = ReturnType<typeof createModelServers>;

/** A status read that never answered (Qt: the failed error kind). */
function failedStatus(why: string): ModelServerStatus {
  const verbatim = (value: string): Text => ({ source: "%1", args: [{ kind: "verbatim", value }] });
  return {
    error: "failed", detail: why, error_label: { source: "Not read" },
    problem: { source: "%1. Zerus tries again only when you ask.", args: [{ kind: "text", value: verbatim(why) }] },
    source: "", models: [], free: -1, total: -1, memory: { source: "" }, low: false, used: 0, raw: [], running: 0, active: [],
  };
}

export const modelServers = createModelServers({
  list: modelServersList, status: modelServerStatus, up: modelServerUp, upResult: modelServerUpResult, sessions: modelServerSessions,
  down: modelServerDown, variant: modelServerVariant, logs: modelServerLogs, save: modelServerSave, remove: modelServerDelete, test: modelServerTest,
  testLogs: modelServerTestLogs, session: modelServerSession,
});
