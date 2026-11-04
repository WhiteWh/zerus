import { Show, Suspense, createSignal, lazy, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listRows } from "./sessions/model";
import { applyBatch, orderedRows } from "../store/catalog";
import { multi, peek, selectedKey, setDetailTab, setMulti, setPeek, setSearch, setSelectedKey, setStateFilter, setStripMode, stateFilter, stripMode } from "../store/ui";
import { handlerFor, route, sequenceState } from "../store/shortcuts";
import { clearMulti } from "./sessions/organize";
import { archive, archiveRows } from "../store/archive";
import { activity } from "../store/session";
import { ARCHIVED_POLL_MS, archiveActivity } from "../store/archiveActivity";
import { markAllRead, markNeedsAttention, markRead } from "../store/marks";
import { copyFolder, copyName, copyCommand, detailsFor, followedFor, lastCopied, copyReference, end, perform, notices } from "../store/actions";
import { panelPlace } from "./sessions/archive-dates";
import { run, running, runMachine } from "../store/batch";
import { machineEntries } from "./sessions/machine-batch";
import { forgetArchived, restoreSession } from "../store/restores";
import { clearArchiveMenuEntry } from "./sessions/selection-live";
import { computerOffline } from "../store/catalog";
import { selectionSnapshot } from "./sessions/selection";
import { snapshotOf, terminateCopy, type ActionRow } from "./sessions/actions-ui";
import type { Action, CreationCard, EditOutcome, Outcome } from "../types";
import { inTauri, metrics, onCreations, pumpStats } from "../bridge";
import { restoreSettings, setSetting } from "../store/settings";
import { motionVisibility, motionPaused, motionUnfocused } from "../store/visibility";
import { setPage, type Page } from "../store/ui";
import { accountsPage } from "../store/accounts";
import { machinesPage } from "../store/machines";
import { projectsPage } from "../store/projects";
import { createProject, defaultProject, deleteProject, grouping, moveProject, moveToProject, renameProject, sortProject } from "../store/organization";
import { activityCandidates, activitySource, type ScenarioResult } from "../perf/scenarios";

// The measurement panel (src/perf), loaded on demand: `?perf=1` in the window
// URL (ZERUS_CLIENT_QUERY=perf=1) or Ctrl+Shift+D. It drives the live session
// list and Activity and registers globalThis.zerusPerf.run().
//
// ZERUS_CLIENT_PERF=auto (or `?perf=auto`) runs it unattended: once the
// catalog has rows and the followed Activity has events, it runs every
// scenario, samples the process tree after 10 s idle, writes the report to
// ZERUS_CLIENT_PERF_OUT (save_perf) and exits the app. The idle windows run
// as displayed, with Animate working sessions off, with Reduce motion (both
// tried without being stored), with the window hidden and then minimised
// (perf_away), each followed by a window as displayed again, all 10 s. Each stage goes to the
// app's stderr (`zerus-client perf: ...`, no session names). A scenario that
// gets no frames ends as skipped after 60 s; the whole run ends after 8 minutes
// with what it has, so a hidden or occluded window never hangs it.

const Diagnostics = lazy(() => import("../perf/Diagnostics"));

interface PerfConfig {
  mode?: string | null;
  out?: string | null;
  label?: string | null;
  representative?: boolean;
  preference?: string;
  display?: string | null;
  actions_smoke?: boolean;
  smoke_file?: string | null;
  /** ZERUS_CLIENT_REPLAY is set: the catalog and Activity come from a synthetic stream. */
  replay?: boolean;
}

/** Resolves once `ready` holds; fails after `ms`. Timers, not frames: rAF stops while a window is hidden. */
function until(ready: () => boolean, ms = 5000, what = "the session's Activity did not appear in time"): Promise<void> {
  const started = performance.now();
  return new Promise((resolve, reject) => {
    const check = () => {
      if (ready()) resolve();
      else if (performance.now() - started > ms) reject(new Error(what));
      else setTimeout(check, 16);
    };
    check();
  });
}

/** One line on the app's stderr in an unattended run. */
function log(line: string) {
  if (inTauri()) void invoke("perf_log", { line }).catch(() => {});
  else console.info(`zerus-client perf: ${line}`);
}

/** The display's refresh rate from the native side (display_hz); null when unknown, then the panel estimates. */
async function displayHz(): Promise<number | null> {
  if (!inTauri()) return null;
  const hz = await invoke<number | null>("display_hz").catch(() => null);
  return typeof hz === "number" && hz > 0 ? hz : null;
}

/** Animation frames delivered within `ms` (a timer ends the count, so none still resolves). */
function countFrames(ms: number): Promise<number> {
  return new Promise((resolve) => {
    let count = 0;
    let running = true;
    const tick = () => { if (!running) return; count++; requestAnimationFrame(tick); };
    requestAnimationFrame(tick);
    setTimeout(() => { running = false; resolve(count); }, ms);
  });
}

/** Animations by play state ("running 0, paused 10"), for the hidden-window log. */
function animationStates(): string {
  const counts = new Map<string, number>();
  for (const animation of document.getAnimations()) counts.set(animation.playState, (counts.get(animation.playState) ?? 0) + 1);
  return [...counts].map(([state, count]) => `${state} ${count}`).join(", ") || "none";
}

/** Running animations by name ("halo x12, zl-spin x1"), for the idle log. */
function runningAnimations(): string {
  const counts = new Map<string, number>();
  for (const animation of document.getAnimations()) {
    if (animation.playState !== "running") continue;
    const name = (animation as CSSAnimation).animationName || animation.constructor.name;
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return [...counts].map(([name, count]) => `${name} x${count}`).join(", ") || "none";
}

/** The process tree over a 10 s window: CPU is the cpu_seconds delta over the wall time between two samples. */
async function idleSample(what: string): Promise<unknown> {
  let sample: unknown;
  try { sample = (await metrics(10_000)) ?? null; } catch (error) { sample = { error: String(error) }; }
  const s = sample as { total?: { cpu_percent?: number; rss_bytes?: number }; processes?: { role?: string; cpu_percent?: number | null }[]; window_ms?: number } | null;
  const roles = (s?.processes ?? []).map((p) => `${p.role} ${p.cpu_percent ?? "n/a"}%`).join(", ");
  log(`${what}: ${s?.total?.cpu_percent ?? "n/a"}% CPU over ${s?.window_ms ?? "n/a"} ms (${roles}), RSS ${Math.round((s?.total?.rss_bytes ?? 0) / 1048576)} MiB`);
  return sample;
}

/** An Activity that scrolls at least this far serves the Activity scroll scenario. */
const SCROLLABLE_PX = 2000;

/** The whole unattended run ends after this, saving what it has. */
const RUN_TIMEOUT_MS = 8 * 60_000;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** Opens the card at `index` of the visible list and waits for its Activity in the DOM. */
async function openSession(index: number) {
  const cards = listRows().filter((r) => r.type === "card");
  if (!cards.length) throw new Error("no sessions in the list");
  const id = cards[index % cards.length].id;
  // Opening the open session again measures a real open, not a no-op.
  if (selectedKey() === id) {
    setSelectedKey(undefined);
    await new Promise(requestAnimationFrame);
  }
  setSelectedKey(id);
  await until(() => activity.target()?.id === id && activity.loaded() && !!document.querySelector(".act-scroll .act-col > :not(.act-label)"));
}

const scrollList = () => document.querySelector<HTMLElement>(".sessions-pane .sp-panel .sp-scroll");
const scrollActivity = () => document.querySelector<HTMLElement>(".act-scroll");

/** Scenario results as they finish, so a run cut short still saves them. */
const finished: ScenarioResult[] = [];

const toJson = (value: unknown) => JSON.stringify(value, (_key, v) => (typeof v === "number" && !Number.isFinite(v) ? null : v), 2);

/** The unattended run: wait for data, run every scenario, save, exit. */
async function autoRun(config: PerfConfig) {
  const begun = performance.now();
  const elapsed = () => `${((performance.now() - begun) / 1000).toFixed(1)} s`;
  let done = false;
  const finish = async (json: string, code: number) => {
    if (done) return;
    done = true;
    if (!inTauri()) { console.info("Zerus Next perf report", json); return; }
    try {
      const where = await invoke<string>("save_perf", { path: config.out ?? "", json });
      log(`saved ${json.length} bytes to ${where}`);
    } catch (error) {
      log(`saving failed: ${String(error)}`);
      code = code || 2;
    }
    log(`exit ${code} after ${elapsed()}`);
    await invoke("exit_app", { code }).catch(() => {});
  };
  const failed = (error: unknown) => finish(toJson({
    schema: "zerus-perf-v1", client: "tauri", error: String(error), created_at: new Date().toISOString(),
    platform: { label: config.label ?? null, representative: config.representative ?? null }, preference: config.preference ?? null,
    scenarios: finished,
  }), 1);
  /** The page while the window was last away (perf_away): its animations and what the pump did until it returned. */
  type Pump = typeof pumpStats;
  type MotionSample = { state: "paused" | "running"; focused: boolean; linux_unfocused: boolean; animations: string };
  let away: { visibility: string; animations?: string; hidden_ms?: number; pump?: Pump; motion?: MotionSample[] } = { visibility: document.visibilityState };
  let atHide: { at: number; pump: Pump } | undefined;
  const onVisibility = () => {
    if (document.visibilityState === "hidden") {
      atHide = { at: performance.now(), pump: { ...pumpStats } };
      away = { ...away, visibility: "hidden", animations: animationStates() };
    } else if (atHide) {
      const before = atHide.pump;
      away.hidden_ms = Math.round(performance.now() - atHide.at);
      away.pump = Object.fromEntries((Object.keys(pumpStats) as (keyof Pump)[]).map((k) => [k, pumpStats[k] - before[k]])) as Pump;
      atHide = undefined;
    }
    log(`visibility ${document.visibilityState} after ${elapsed()}${away.animations && document.visibilityState === "hidden" ? `; animations ${away.animations}` : ""}`);
  };
  document.addEventListener("visibilitychange", onVisibility);
  const onMotion = () => {
    const sample: MotionSample = { state: motionPaused() ? "paused" : "running", focused: document.hasFocus(), linux_unfocused: motionUnfocused(), animations: animationStates() };
    away.motion = [...(away.motion ?? []).slice(-63), sample];
    log(`motion ${sample.state} after ${elapsed()}; document focus ${sample.focused}; Linux toplevel unfocused ${sample.linux_unfocused}; animations ${sample.animations}`);
  };
  const stopMotion = motionVisibility.onChange(onMotion);
  const cutoff = setTimeout(() => { log(`run timeout after ${elapsed()}, saving ${finished.length} scenarios`); void failed("run timeout (8 min)"); }, RUN_TIMEOUT_MS);
  try {
    log(`start (display ${config.display ?? "n/a"}, preference ${config.preference ?? "n/a"}, visibility ${document.visibilityState}, cross-origin isolated ${crossOriginIsolated})`);
    await until(() => orderedRows().length > 0, 180_000, "the catalog stayed empty for 3 minutes");
    log(`rows ready: ${orderedRows().length} after ${elapsed()}`);
    // Many sessions keep no journal events; move on until one has some (the
    // Activity scenarios need them). A chosen `?select=` session is tried first.
    const cards = listRows().filter((r) => r.type === "card").map((r) => r.id);
    // A live run tries 16 at most; a replay run tries every card, so the fixture's
    // long session is found whatever the list order and the run reaches SCROLLABLE_PX.
    const replay = !!config.replay;
    const candidates = activityCandidates(selectedKey(), cards, replay);
    // The Activity scenario needs a transcript that scrolls: read older pages
    // of a short one, and keep the session that scrolls furthest.
    const range = () => { const el = scrollActivity(); return el ? el.scrollHeight - el.clientHeight : 0; };
    let tried = 0;
    let best: { id: string; range: number; pages: number } | undefined;
    /** Reads up to `pages` older pages, fewer once the Activity scrolls far enough. */
    const readOlder = async (pages: number, enough: number) => {
      let read = 0;
      while (read < pages && activity.events().length > 0 && range() < enough && !activity.olderDone()) {
        activity.loadOlder();
        await until(() => !activity.loadingOlder(), 8000).catch(() => {});
        await sleep(150);
        read++;
      }
      await sleep(300);
      return read;
    };
    for (const id of candidates) {
      tried++;
      setSelectedKey(id);
      const kind = `${id.startsWith("\n") ? "this computer" : "a peer"}, ${id.includes("\nrun:") ? "run key" : "name key"}`;
      await until(() => activity.target()?.id === id && (activity.loaded() || activity.events().length > 0), 15_000).catch(() => {});
      // Events can follow the first (empty) answer by a moment.
      await until(() => activity.events().length > 0, 1500).catch(() => {});
      const pages = await readOlder(4, SCROLLABLE_PX);
      const error = activity.error();
      log(`selected ${tried}/${candidates.length}: ${kind}; events ${activity.events().length}; scrolls ${range()} px${error ? `; follow failed: ${error}` : ""}`);
      if (activity.events().length > 0 && (!best || range() > best.range)) best = { id, range: range(), pages };
      if (best && best.range >= SCROLLABLE_PX) break;
    }
    if (best && selectedKey() !== best.id) {
      const chosen = best;
      setSelectedKey(chosen.id);
      await until(() => activity.target()?.id === chosen.id && activity.events().length > 0, 15_000).catch(() => {});
      await readOlder(chosen.pages, chosen.range);
    }
    log(`activity events: ${activity.events().length}, scrolls ${range()} px after ${elapsed()} (${tried} sessions tried${replay ? ", replay" : ""})`);
    const source = activitySource(replay, best && { events: activity.events().length, range: range(), pages: best.pages });
    log(`activity source: ${replay ? "replay" : "live"}, ${source.events} events, ${source.scroll_px} px, ${source.older_pages} older pages`);
    // Without animation frames every pacing scenario would time out: say so at once.
    const frames = await countFrames(3000);
    log(`frames: ${frames} in 3 s (visibility ${document.visibilityState}, focus ${document.hasFocus()}, window ${innerWidth}x${innerHeight} at ${devicePixelRatio}x, display ${await displayHz() ?? "unknown"} Hz, cross-origin isolated ${crossOriginIsolated})`);
    await until(() => !!globalThis.zerusPerf, 30_000, "the Diagnostics panel did not start");
    const report = await globalThis.zerusPerf!.run();
    log(`scenarios done after ${elapsed()}; settling 10 s, then seven 10 s idle windows`);
    await sleep(10_000);
    // Idle as displayed (working halos, header pulses, the mark while a session
    // needs attention keep producing frames), then with each motion setting, so
    // a report separates what the motion costs from what the app does. The
    // settings are tried without being stored.
    log(`running animations: ${runningAnimations()}`);
    const idle = await idleSample("idle");
    void setSetting("workspace/animateActivity", false, { persist: false });
    await sleep(1000);
    log(`running animations with Animate working sessions off: ${runningAnimations()}`);
    const still = await idleSample("idle with Animate working sessions off");
    restoreSettings();
    void setSetting("workspace/reduceMotion", true, { persist: false });
    await sleep(1000);
    log(`running animations with Reduce motion: ${runningAnimations()}`);
    const calm = await idleSample("idle with Reduce motion");
    restoreSettings();
    await sleep(1000);
    // Hidden, then minimised: the native side takes the window away, samples
    // the process tree and brings it back. The pump and the motion must stop.
    const gone: Record<string, unknown> = {};
    for (const how of ["hide", "minimize"] as const) {
      away = { visibility: document.visibilityState };
      const result = await invoke<{ sample?: unknown; minimized?: boolean }>("perf_away", { how, window_ms: 10_000 }).catch((error) => ({ error: String(error) }));
      // The page reports its return a moment after the window is back.
      await until(() => document.visibilityState === "visible" && (away.visibility !== "hidden" || !!away.pump), 3000).catch(() => {});
      const s = (result as { sample?: { total?: { cpu_percent?: number }; processes?: { role?: string; cpu_percent?: number | null }[]; window_ms?: number } }).sample;
      const roles = (s?.processes ?? []).map((p) => `${p.role} ${p.cpu_percent ?? "n/a"}%`).join(", ");
      const pump = away.pump ? Object.entries(away.pump).map(([k, v]) => `${k} +${v}`).join(", ") : "n/a";
      const motion = away.motion?.map((sample) => `${sample.state} (focus ${sample.focused}, animations ${sample.animations})`).join("; ") || "no change";
      const page = `${away.visibility === "hidden" ? `hidden for ${away.hidden_ms ?? "n/a"} ms, animations ${away.animations}, pump while hidden ${pump}` : `stayed ${away.visibility}`}; motion ${motion}`;
      log(`window ${how}: ${s?.total?.cpu_percent ?? "n/a"}% CPU over ${s?.window_ms ?? "n/a"} ms (${roles}); page ${page}${"minimized" in result ? `; minimized ${result.minimized}` : ""}${"error" in result ? `; ${(result as { error: string }).error}` : ""}`);
      await sleep(1000);
      // As displayed again: the motion resumed and the pump caught up.
      log(`running animations after the window returned: ${runningAnimations()}`);
      gone[how] = { ...result, page: away, returned: await idleSample(`idle after ${how}`) };
    }
    report.resources = { ...(report.resources ?? {}), idle, idle_still_working: still, idle_reduced_motion: calm, idle_away: gone };
    clearTimeout(cutoff);
    await finish(toJson({ ...report, preference: config.preference ?? null, activity_source: source }), 0);
  } catch (error) {
    clearTimeout(cutoff);
    log(`failed: ${String(error)}`);
    await failed(error);
  } finally {
    stopMotion();
    document.removeEventListener("visibilitychange", onVisibility);
  }
}

// `?actions-smoke=1` (ZERUS_CLIENT_QUERY=actions-smoke=1, with ZERUS_CLIENT_PERF
// set to any value but auto so that exit_app is allowed) is the end-to-end check
// of the guarded actions: with ZERUS_CLIENT_REPLAY supplying the catalog and
// ZERUS_CLIENT_HGS a fake hgs, it selects the replay session (its details come
// from the Activity state value, or one inspect), runs start fresh, forget and
// forget binding (through `perform`, as the dialogs confirm), pause, resume, rename,
// archive, the end call the Terminate dialog picks for it through the
// dialog's confirmation (`end`), then the other end call (terminate or kill)
// once each, and exits 0 (1 when an outcome failed). Before the writes it marks
// the session read, marks all as read (logging the count) and marks the session
// as needing attention last; `?actions-smoke=marks` then only waits for the
// session and reports whether that review mark survived a restart. Before them it
// switches to the Archive filter, logs the archive page's status text and row count,
// checks that the date panel opens in the page's body at its button while the list
// panel is transformed, and selects the archived row, waiting for its details
// (`inspect --archive ID`).
const SMOKE_SESSION = "claude/sample-project/review";
const SMOKE_RENAMED = "claude/sample-project/review-renamed";

async function marksAfterRestart(allowed: boolean) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  if (!allowed) {
    log("actions-smoke not started: it needs ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_HGS and ZERUS_CLIENT_PERF");
    return;
  }
  try {
    await until(() => orderedRows().some((s) => s.name === SMOKE_SESSION && s.runId), 30_000, "the replay session did not appear");
    // The stored marks load a moment after the catalog; give them time to apply.
    const review = await until(() => !!orderedRows().find((s) => s.name === SMOKE_SESSION)?.reviewLater, 5_000, "no review mark").then(() => true, () => false);
    log(`actions-smoke marks-after-restart: review ${review}`);
    await exit(review ? 0 : 1);
  } catch (error) {
    log(`actions-smoke failed: ${String(error)}`);
    await exit(1);
  }
}

/**
 * Opens the Archive bar's date panel while the list panel carries a transform
 * (as the strip's peek overlay does): the panel must be in the page's body,
 * placed at the picker button in viewport coordinates, and Escape must close
 * it with the focus back on the button.
 */
async function datePanelPlacement(): Promise<{ ok: boolean; text: string }> {
  const list = document.querySelector<HTMLElement>(".sp-panel");
  const picker = document.querySelector<HTMLButtonElement>(".archive-bar .archive-period");
  if (!list || !picker) return { ok: false, text: "failed (no Archive bar)" };
  list.style.transform = "translateX(37px)";
  try {
    picker.click();
    const pop = () => document.querySelector<HTMLElement>(".archive-pop");
    const want = () => panelPlace(picker.getBoundingClientRect(), pop()?.getBoundingClientRect().height ?? 0, { width: innerWidth, height: innerHeight });
    const at = () => { const r = pop()?.getBoundingClientRect(); return r ? { left: r.left, top: r.top } : undefined; };
    const placed = await until(() => { const r = at(), w = want(); return !!r && Math.abs(r.left - w.left) < 1 && Math.abs(r.top - w.top) < 1; }, 3000, "misplaced")
      .then(() => true, () => false);
    const outside = !!pop() && !pop()!.closest(".sp-panel");
    const r = at(), w = want();
    pop()?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    const closed = await until(() => !pop(), 2000, "still open").then(() => true, () => false);
    const focused = document.activeElement === picker;
    const ok = placed && outside && closed && focused;
    return { ok, text: `${ok ? "done" : "failed"} (in the body ${outside}, at ${r ? `${Math.round(r.left)},${Math.round(r.top)}` : "none"} for ${Math.round(w.left)},${Math.round(w.top)}, closed ${closed}, focus on the picker ${focused})` };
  } finally {
    list.style.transform = "";
  }
}

/**
 * The date panel over a hover-opened strip peek. Qt keeps a hover peek open
 * while a popup is active (SessionPanelDock.h 50-56) and its popup keeps the
 * keyboard: Shift+Tab and Tab wrap inside the panel; Escape with the focus
 * outside the panel closes only the panel (the peek and a selection stay);
 * the pointer leaving the list while the panel is open keeps the peek; once
 * the panel closed, the next leave closes it.
 */
async function peekWithDatePanel(): Promise<{ ok: boolean; text: string }> {
  const sleep = (ms: number) => new Promise((done) => setTimeout(done, ms));
  const pop = () => document.querySelector<HTMLElement>(".archive-pop");
  const key = (name: string, shiftKey = false, target: EventTarget = document.activeElement ?? document.body) =>
    target.dispatchEvent(new KeyboardEvent("keydown", { key: name, shiftKey, bubbles: true, cancelable: true }));
  const wasStrip = stripMode();
  setPeek(false);
  setStripMode(true);
  try {
    await until(() => !!document.querySelector(".strip-col"), 2000, "no strip").catch(() => {});
    const section = document.querySelector<HTMLElement>("section.sessions-pane");
    // Each part starts from a peek the hover opened, so one failure does not hide another.
    const hover = async () => {
      if (peek()) return true;
      document.querySelector(".strip-col")?.dispatchEvent(new PointerEvent("pointerenter"));
      return until(() => peek(), 2000, "no peek").then(() => true, () => false);
    };
    const hovered = await hover();
    const picker = document.querySelector<HTMLButtonElement>(".sp-panel .archive-bar .archive-period");
    if (!hovered || !section || !picker) return { ok: false, text: `failed (the hover opened the peek ${hovered}, picker ${!!picker})` };
    const open = async () => { picker.click(); return until(() => document.activeElement?.id === "archive-from", 2000, "not open").then(() => true, () => false); };
    const opened = await open();
    const controls = [...(pop()?.querySelectorAll<HTMLElement>("input:not([disabled]), button:not([disabled])") ?? [])];
    key("Tab", true);
    const back = !!controls.length && document.activeElement === controls.at(-1);
    key("Tab");
    const forward = !!controls.length && document.activeElement === controls[0];
    // Escape with the focus outside the panel and a selection: only the panel closes.
    const rows = orderedRows().slice(0, 2);
    for (const r of rows) setMulti(r.key, true);
    (document.activeElement as HTMLElement | null)?.blur();
    key("Escape", false, document.body);
    await sleep(50);
    const escaped = !pop() && peek() && rows.length === 2 && rows.every((r) => !!multi[r.key]);
    clearMulti();
    // Leaving while the panel is open keeps the peek.
    const reopened = (await hover()) && (await open());
    section.dispatchEvent(new PointerEvent("pointerleave"));
    await sleep(800);
    const kept = peek() && !!pop();
    key("Escape");
    section.dispatchEvent(new PointerEvent("pointerenter"));
    await sleep(500);
    const stayed = peek() && !pop();
    // With the panel closed, the next leave closes the peek.
    section.dispatchEvent(new PointerEvent("pointerleave"));
    const closed = await until(() => !peek(), 2000, "kept").then(() => true, () => false);
    const ok = opened && back && forward && escaped && reopened && kept && stayed && closed;
    return {
      ok,
      text: `${ok ? "done" : "failed"} (Shift+Tab wraps ${back}, Tab wraps ${forward}, Escape outside the panel closed only the panel ${escaped}, `
        + `leaving with the panel open kept the peek ${kept}, back inside it stayed ${stayed}, the next leave closed it ${closed})`,
    };
  } finally {
    pop()?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    clearMulti();
    setPeek(false);
    setStripMode(wasStrip);
  }
}

async function actionsSmoke(allowed: boolean) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  if (!allowed) {
    log("actions-smoke not started: it needs ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_HGS and ZERUS_CLIENT_PERF");
    return;
  }
  try {
    await until(() => orderedRows().some((s) => s.name === SMOKE_SESSION && s.runId), 30_000, "the replay session did not appear");
    const row = (): ActionRow => {
      const s = orderedRows().find((r) => r.name === SMOKE_SESSION);
      if (!s) throw new Error("the replay session is gone");
      return snapshotOf(s);
    };
    setSelectedKey(row().key);
    const chosen = terminateCopy(row()).action;
    log(`actions-smoke end call the Terminate dialog picks: ${chosen}`);
    // The Archive view first: the fake answers `archives --json` with one archived row.
    // Its status text and row count are logged, then the row is selected so that its
    // details are read with `inspect --archive ID` (the argv is in the fake's log.ndjson).
    const keep = row().key;
    let bad = 0;
    setStateFilter("archived");
    await until(() => archive.pages()[archive.computers()[0]?.computer ?? ""]?.loaded === true, 15_000, "the archive page did not load");
    log(`actions-smoke archive-page: ${archive.status().summary} (${archiveRows().length} rows)`);
    const archivedKey = archive.rows()[0]?.key;
    if (!archivedKey) throw new Error("the archive page has no row");
    const panel = await datePanelPlacement();
    log(`actions-smoke archive-date-panel: ${panel.text}`);
    if (!panel.ok) bad++;
    const hover = await peekWithDatePanel();
    log(`actions-smoke archive-date-panel-peek: ${hover.text}`);
    if (!hover.ok) bad++;
    setSelectedKey(archivedKey);
    // The details the header and the menus read, from `inspect --archive ID`; bounded.
    const archived = archive.row(archivedKey)!;
    const inspected = await until(() => !!detailsFor(archived, !computerOffline(archived.computer), followedFor(archivedKey)), 15_000, "no details")
      .then(() => true, () => false);
    log(`actions-smoke archive-inspect: ${inspected ? "done" : "failed (no details within 15 s)"} ${archived.archiveId ?? "(no id)"}`);
    if (!inspected) bad++;
    {
      // Its recorded Activity: the fake's `inspect` entry answers synthetic events, read through the
      // archived store (never the live follow) and shown read-only, with no notice or live line.
      const pane = () => document.querySelector(".detail .activity");
      const items = () => pane()?.querySelectorAll("[id^='item-']").length ?? 0;
      const shown = await until(() => archiveActivity.events().length > 0 && items() > 0, 15_000, "no archived Activity").then(() => true, () => false);
      const problem = !shown ? "no events shown within 15 s"
        : activity.target()?.id === archivedKey ? "the archived row is followed live"
        : pane()?.querySelector(".notice, .act-live") ? "a notice or a live line shows" : "";
      log(`actions-smoke archive-activity: ${problem ? `failed (${problem})` : `done (${archiveActivity.events().length} events)`}`);
      if (problem) bad++;
      // One poll, so the fake's log shows the next read after the reply's cursor.
      await sleep(ARCHIVED_POLL_MS + 500);
    }
    {
      // The archived process inventory is one details read with processes included,
      // never a live processes topic follow.
      await setSetting("processes/enabled", true);
      setDetailTab("processes");
      const shown = await until(() => document.querySelector(".proc-tab")?.textContent?.includes("cargo test --workspace") === true,
        15_000, "no archived process inventory").then(() => true, () => false);
      log(`actions-smoke archive-processes: ${shown ? "done" : "failed (no inventory within 15 s)"}`);
      if (!shown) bad++;
      setDetailTab("activity");
      await setSetting("processes/enabled", false);
    }
    // Restore, Forget archived and Clear archive on the loaded archived row. The fake answers
    // `resume NAME --archive ID -d` and `kill NAME --archive ID` with a plain success (its log.ndjson
    // holds both argv lines); the live row never appears, so the restore stays pending, as in Qt.
    const errorsSince = (id: number) => notices().filter((n) => n.id > id && n.tone === "error").map((n) => n.text);
    const lastNoticeId = () => notices().reduce((m, n) => Math.max(m, n.id), 0);
    // Pages load only while the Archive filter is shown, so a user looks at it again after the restore.
    const settled = async () => {
      setStateFilter("archived");
      await sleep(300);
      await until(() => archive.pages()[archive.computers()[0]?.computer ?? ""]?.loaded === true && !!archive.row(archivedKey), 15_000, "the archive did not reload");
    };
    {
      const before = lastNoticeId();
      await restoreSession(snapshotOf(archived));
      const errors = errorsSince(before);
      // A restore that was not enabled returns silently and leaves the Archive filter on.
      const switched = stateFilter() === "all";
      log(`actions-smoke restore: ${errors.length || !switched ? `failed (${errors.join(" / ") || "the entry did not run"})` : "done"} (state filter now ${stateFilter()})`);
      if (errors.length || !switched) bad++;
      await settled();
    }
    {
      const before = lastNoticeId();
      await forgetArchived(snapshotOf(archive.row(archivedKey)!));
      const errors = errorsSince(before);
      log(`actions-smoke forget-archived: ${errors.length ? `failed (${errors.join(" / ")})` : "done"}`);
      if (errors.length) bad++;
      await settled();
    }
    {
      // Through the menu entry the batch menu shows; its confirmation dialog is answered by pressing its own button.
      // The fake's archived row is built to be forgettable, so a disabled entry is a failure.
      const entry = clearArchiveMenuEntry();
      if (!("label" in entry) || entry.disabled) {
        log(`actions-smoke clear-archive: failed (${"label" in entry ? entry.label : "no entry"} is disabled)`);
        bad++;
      } else {
        const before = lastNoticeId();
        entry.action?.();
        const button = (): HTMLButtonElement | undefined =>
          [...document.querySelectorAll<HTMLButtonElement>("dialog[open] button.solid-danger")].find((b) => b.textContent === "Clear archive");
        const asked = await until(() => !!button(), 5000, "no confirmation").then(() => true, () => false);
        if (!asked) { log("actions-smoke clear-archive: failed (the confirmation did not open)"); bad++; }
        else {
          button()!.click();
          await until(() => !running() && notices().some((n) => n.id > before && /\d+ of \d+ completed/.test(n.text)), 15_000, "the batch did not end")
            .then(() => {}, () => {});
          const fresh = notices().filter((n) => n.id > before);
          const failed = fresh.some((n) => n.tone === "error") || !fresh.length;
          log(`actions-smoke clear-archive: ${failed ? "failed" : "done"} (${fresh.map((n) => n.text).join(" / ")})`);
          if (failed) bad++;
          await settled().catch(() => {});
        }
      }
    }
    setStateFilter("all");
    setSelectedKey(keep);
    const local = (ok: boolean): Outcome => (ok ? { outcome: "done", receipt: null, cleanup: null } : { outcome: "failed", detail: "the row changed" });
    const steps: [string, () => Promise<Outcome>][] = [
      ["mark as read", async () => local(markRead(row()))],
      ["mark all as read", async () => { log(`actions-smoke mark-all-as-read: ${markAllRead()} marked`); return local(true); }],
      ["mark needs attention", async () => local(markNeedsAttention(row()))],
      ["copy session reference", () => copyReference(row())],
      ["copy connect command", () => copyCommand(row())],
      ["copy session name", () => copyName(row())],
      ["copy folder path", () => copyFolder(row())],
      ["start fresh", () => perform(row(), { kind: "start_fresh" })],
      ["forget", () => perform(row(), { kind: "forget" })],
      ["forget binding", () => perform(row(), { kind: "forget_binding" })],
      ...([{ kind: "pause" }, { kind: "resume" }, { kind: "rename", new_name: SMOKE_RENAMED, archive_id: null }, { kind: "archive" }] as Action[])
        .map((action): [string, () => Promise<Outcome>] => [action.kind, () => perform(row(), action)]),
      [chosen, () => end(row(), chosen)],
      ...(["terminate", "kill"] as const).filter((kind) => kind !== chosen)
        .map((kind): [string, () => Promise<Outcome>] => [kind, () => perform(row(), { kind })]),
    ];
    // Batch steps go through the store the selection bar uses and the helper the batch menu uses.
    const live = () => {
      const s = orderedRows().find((r) => r.name === SMOKE_SESSION);
      if (!s) throw new Error("the replay session is gone");
      return s;
    };
    const lastId = () => notices().reduce((m, n) => Math.max(m, n.id), 0);
    const since = (id: number) => notices().filter((n) => n.id > id);
    const lastNotice = () => notices().at(-1)?.text ?? "(no notice)";
    for (const action of ["review_later", "mark_read"] as const) {
      const before = lastId();
      await run(action, [selectionSnapshot(live())]);
      const text = since(before).length ? since(before).map((n) => n.text).join(" / ") : "(skipped: the action does not apply to the replay row by Qt's rules)";
      log(`actions-smoke batch-${action.replace("_", "-")}: ${text}`);
    }
    for (const kind of ["pause_all", "resume_all"] as const) {
      const row = live();
      const entry = machineEntries({
        machine: row.machine || "this computer", computer: row.computer, online: !computerOffline(row.computer), batchRunning: running(),
        rows: orderedRows().filter((r) => r.computer === row.computer),
        start: () => {},
      }).find((e) => "label" in e && e.label.startsWith(kind === "pause_all" ? "Pause all" : "Resume all"));
      const name = kind === "pause_all" ? "pause-all" : "resume-all";
      if (!entry || !("label" in entry) || entry.disabled) { log(`actions-smoke ${name}: skipped (${entry && "label" in entry ? entry.label : "no entry"} is disabled for the replay row by Qt's rules)`); continue; }
      const count = Number(/\((\d+)\)/.exec(entry.label)?.[1] ?? 0);
      const before = lastId();
      await runMachine(row.computer, row.machine || "this computer", kind, count);
      const fresh = since(before);
      const failed = fresh.some((n) => n.tone === "error");
      log(`actions-smoke ${name}: ${failed ? "failed" : "done"} (${fresh.map((n) => n.text).join(" / ")})`);
      if (failed) bad++;
    }
    for (const [kind, step] of steps) {
      const out = await step();
      const text = kind.startsWith("copy ") && out.outcome === "done" ? ` ${lastCopied()}` : "";
      log(`actions-smoke ${kind.startsWith("copy ") ? `copy-${kind.slice(5).replace(/ /g, "-")}` : kind.replace(/ /g, "-")}: ${out.outcome}${out.outcome === "failed" ? ` (${out.detail})` : ""}${text}`);
      if (out.outcome === "failed") bad++;
    }
    await exit(bad ? 1 : 0);
  } catch (error) {
    log(`actions-smoke failed: ${String(error)}`);
    await exit(1);
  }
}

// `?newsession-smoke=1` runs New session over the replay's project folder and
// the fake creation hgs: read-only open/Cancel, None through Ctrl+N and Enter,
// and Auto on a verified Git catalog. Bound events prove exact-row adoption.
async function newSessionSmoke(allowed: boolean) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  if (!allowed) {
    log("newsession-smoke not started: it needs ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_HGS and ZERUS_CLIENT_PERF");
    return;
  }
  let stopAdmit = () => {}, stopEvents = () => {};
  try {
    const view = await import("./sessions/newsession-view");
    const loaded = await view.loadNewSession();
    await until(() => grouping().ready, 15_000, "the replay organization was not ready");
    const seen = new Map<string, CreationCard>();
    let admitted = "", pass = 0;
    stopEvents = await onCreations((event) => {
      if ("removed" in event) return;
      seen.set(event.id, event);
      log(`newsession-smoke ${pass} card ${event.id} ${event.state} ${event.caption}`);
    });
    stopAdmit = loaded.onCreationAdmitted((id) => { admitted = id; });
    const openDialog = async (shortcut: boolean) => {
      if (shortcut) {
        const first = orderedRows()[0];
        if (!first) throw new Error("the replay has no project session");
        setSelectedKey(first.key);
        await until(() => handlerFor("session.new", document) !== undefined, 5000, "session.new was not registered");
        const pressed = new KeyboardEvent("keydown", { key: "n", code: "KeyN", ctrlKey: true, bubbles: true, cancelable: true });
        log(`newsession-smoke ${pass} Ctrl+N route ${JSON.stringify(route(pressed, sequenceState()))}`);
        (document.activeElement ?? document.body).dispatchEvent(pressed);
      } else {
        await loaded.openNewSession({ project: "p-sample-project", folder: "folder-smoke" });
      }
      await until(() => loaded.newSession() !== undefined, 15_000, `New session did not open: ${notices().at(-1)?.text ?? "no notice"}`);
      await until(() => loaded.newSession() !== undefined && !!document.querySelector(".ns-form"), 15_000, "New session did not render");
      const actions = loaded.newSessionActions;
      if (loaded.newSession()!.folders.length > 0) actions.chooseFolder(0);
      await until(() => loaded.newSession()?.accounts.some((row) => row.provider === "claude") === true, 15_000, "the account catalog did not load");
      const account = loaded.newSession()!.accounts.findIndex((row) => row.provider === "claude");
      if (account < 0) throw new Error("the fake Claude account is missing");
      actions.chooseAccount(account);
      await until(() => loaded.newSession()?.form !== undefined, 15_000, "the form did not settle");
    };
    const start = async (keyboard: boolean) => {
      admitted = "";
      const actions = loaded.newSessionActions;
      actions.setAuto(!keyboard);
      actions.setName(`newsession-smoke-${pass}`);
      await until(() => loaded.newSession()?.form?.start_enabled === true, 20_000, "Start stayed disabled");
      log(`newsession-smoke ${pass} start ${Date.now()}`);
      if (keyboard) {
        const name = document.querySelector<HTMLElement>("#ns-name");
        if (!name) throw new Error("the session name field is missing");
        name.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
      } else {
        await loaded.startNewSession(async () => true);
      }
      await until(() => admitted !== "", 20_000, "the creation was not admitted");
      await until(() => seen.get(admitted)?.state === "bound", 45_000, "the creation did not bind");
      const card = seen.get(admitted)!;
      const row = orderedRows().find((s) => s.computer === card.host && s.name === card.bound_session);
      if (!row) throw new Error("the bound session row is missing");
      if (row.machine !== card.machine) throw new Error(`machine mismatch ${row.machine} != ${card.machine}`);
      if (selectedKey() !== row.key) throw new Error("the exact bound row was not adopted");
      log(`newsession-smoke ${pass} bound row-present true adopted true machine ${card.machine}`);
      log(`newsession-smoke ${pass} end ${Date.now()}`);
    };

    pass = 1;
    log(`newsession-smoke ${pass} begin ${Date.now()}`);
    await openDialog(false);
    await until(() => loaded.newSession()?.form?.choose_existing_enabled === true, 15_000, "Choose existing did not become enabled");
    const options = document.querySelector<HTMLButtonElement>("#ns-worktree-options");
    if (!options) throw new Error("the worktree options button is missing");
    options.click();
    await until(() => {
      const choose = [...document.querySelectorAll<HTMLButtonElement>(".menu-item")].find((button) => button.textContent === "Choose existing…");
      if (!choose) return false;
      choose.click();
      return true;
    }, 5000, "Choose existing is missing");
    await until(() => [...document.querySelectorAll("dialog[open]")].some((dialog) => dialog.textContent?.includes("Choose an existing worktree")), 10_000, "Choose existing did not open");
    const child = [...document.querySelectorAll<HTMLDialogElement>("dialog[open]")].find((dialog) => dialog.textContent?.includes("Choose an existing worktree"));
    const cancel = child ? [...child.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "Cancel") : undefined;
    if (!cancel) throw new Error("Choose existing has no Cancel");
    cancel.click();
    loaded.newSessionActions.setSource(true);
    await until(() => loaded.newSession()?.repository === true, 5000, "the repository source did not open");
    loaded.newSessionActions.setSource(false);
    await until(() => loaded.newSession()?.repository === false, 5000, "the folder source did not return");
    loaded.closeNewSession();
    await until(() => !document.querySelector("dialog[open]"), 5000, "New session did not close");
    log(`newsession-smoke ${pass} end ${Date.now()}`);

    pass = 2;
    log(`newsession-smoke ${pass} begin ${Date.now()}`);
    await openDialog(true);
    await start(true);

    pass = 3;
    log(`newsession-smoke ${pass} begin ${Date.now()}`);
    await openDialog(false);
    await start(false);
    stopAdmit();
    stopEvents();
    await exit(0);
  } catch (error) {
    stopAdmit();
    stopEvents();
    log(`newsession-smoke failed: ${String(error)}`);
    await exit(1);
  }
}

// `?screens-smoke=1` (ZERUS_CLIENT_QUERY=screens-smoke=1, in the environment
// the actions smoke needs) shows Overview, Machines, Accounts and Projects in
// turn on the replay catalog with a fake hgs answering the reads. It waits
// until each view loaded, logs "screens-smoke <screen>: shown", holds the page
// 3 s (for a screenshot) and exits 0, or 1 when a view did not load in 30 s.
async function screensSmoke(allowed: boolean) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  if (!allowed) {
    log("screens-smoke not started: it needs ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_HGS and ZERUS_CLIENT_PERF");
    return;
  }
  const steps: [Page, string, () => boolean][] = [
    ["dashboard", "overview", () => accountsPage().snapshot && accountsPage().overview.length > 0],
    ["machines", "machines", () => machinesPage().loaded],
    ["accounts", "accounts", () => accountsPage().snapshot && accountsPage().pending === 0],
    ["projects", "projects", () => projectsPage().loaded],
  ];
  try {
    for (const [target, name, ready] of steps) {
      setPage(target);
      await until(ready, 30_000, `${name} did not load`);
      log(`screens-smoke ${name}: shown`);
      await new Promise((resolve) => setTimeout(resolve, 3000));
    }
    await exit(0);
  } catch (error) {
    log(`screens-smoke failed: ${String(error)}`);
    await exit(1);
  }
}

// `?organization-smoke=1` (ZERUS_CLIENT_QUERY=organization-smoke=1, in the
// environment the actions smoke needs, with ZERUS_CLIENT_HGS a fake hgs that
// answers `swarm apply` with the replay's organization) runs one edit of each
// project menu through the store the menus use, once the replay's
// organization is ready and lists the replay session. Each step names a
// project of the fixture, which every fake reply lists again. It logs
// "organization-smoke <step>: <outcome>" and exits 0 when every outcome is
// done, else 1. Sorting a project whose rows are all listed members changes
// only this device's layout, so it sends no `swarm apply`.
async function organizationSmoke(allowed: boolean) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  if (!allowed) {
    log("organization-smoke not started: it needs ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_HGS and ZERUS_CLIENT_PERF");
    return;
  }
  try {
    const row = () => {
      const s = orderedRows().find((r) => r.name === SMOKE_SESSION);
      if (!s) throw new Error("the replay session is gone");
      return s;
    };
    await until(() => grouping().ready && orderedRows().some((s) => s.name === SMOKE_SESSION), 30_000, "the replay organization was not ready in time");
    const steps: [string, () => Promise<EditOutcome>][] = [
      ["move-to-project", () => moveToProject([row()], "p-aurora")],
      ["create-project", () => createProject("Smoke project", [row()])],
      ["rename-project", () => renameProject("p-docs-site", "Docs renamed")],
      ["sort-project", () => sortProject("p-sample-project")],
      ["move-project", () => moveProject("p-orders-api", "p-sample-project")],
      ["delete-project", () => deleteProject("p-docs-site", "ungrouped")],
    ];
    let bad = 0;
    for (const [step, run] of steps) {
      const out = await run();
      log(`organization-smoke ${step}: ${out.outcome}${out.outcome === "done" ? "" : ` (${out.detail})`}`);
      if (out.outcome !== "done") bad++;
    }
    await exit(bad ? 1 : 0);
  } catch (error) {
    log(`organization-smoke failed: ${String(error)}`);
    await exit(1);
  }
}

// `?background-smoke=1`: the list background menu and its New project dialog on the replay.
async function backgroundSmoke(allowed: boolean) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  if (!allowed) {
    log("background-smoke not started: it needs ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_HGS and ZERUS_CLIENT_PERF");
    return;
  }
  try {
    await until(() => grouping().ready && orderedRows().length > 0, 30_000, "the replay organization was not ready");
    const panel = document.querySelector<HTMLElement>(".sp-panel");
    if (!panel) throw new Error("no Sessions panel");
    panel.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 320, clientY: 240 }));
    const clear = clearArchiveMenuEntry();
    const clearLabel = "label" in clear ? clear.label : "";
    const labels = ["New session…", "New project…", "Expand all projects", "Collapse all projects", "Expand all collapsed", "Mark all as read", clearLabel];
    await until(() => !!document.querySelector(".menu") && labels.every((text) => document.querySelector(".menu")?.textContent?.includes(text)), 3000, "the background menu did not list every entry");
    const newProject = [...document.querySelectorAll<HTMLButtonElement>(".menu button")].find((button) => button.textContent?.trim() === "New project…");
    if (!newProject) throw new Error("New project is missing");
    newProject.click();
    await until(() => document.querySelector("#org-dlg-title")?.textContent === "New project", 3000, "the New project dialog did not open");
    const cancel = [...document.querySelectorAll<HTMLButtonElement>("dialog[open] button")].find((button) => button.textContent?.trim() === "Cancel");
    if (!cancel) throw new Error("the New project dialog has no Cancel button");
    cancel.click();
    await until(() => !document.querySelector("dialog[open]"), 2000, "the New project dialog did not close");
    const collapse = async (label: string) => {
      panel.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 320, clientY: 240 }));
      let button: HTMLButtonElement | undefined;
      await until(() => {
        button = [...document.querySelectorAll<HTMLButtonElement>(".menu button")].find((candidate) => candidate.textContent?.trim() === label);
        if (!button) throw new Error(`${label} is missing`);
        return true;
      }, 3000, `${label} did not appear`);
      button!.click();
    };
    await collapse("Collapse all projects");
    await until(() => grouping().headers.every((header) => header.collapsed), 3000, "Collapse all projects did not apply");
    await collapse("Expand all projects");
    await until(() => grouping().headers.every((header) => !header.collapsed), 3000, "Expand all projects did not apply");
    const row = document.querySelector<HTMLElement>(".sp-scroll .rowhit");
    if (!row) throw new Error("no session row");
    row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 320, clientY: 240 }));
    await until(() => !!document.querySelector(".menu"), 3000, "the session menu did not open");
    const compact = [...document.querySelectorAll<HTMLButtonElement>(".menu button")].find((button) => button.textContent?.trim() === "Compact session row");
    if (!compact) throw new Error("Compact session row is missing");
    compact.click();
    await until(() => !!document.querySelector(".scard.compact"), 3000, "the session row did not compact");
    const rail = document.querySelector<HTMLElement>(".rail-btn");
    if (!rail) throw new Error("no Sessions rail button");
    rail.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 40, clientY: 240 }));
    let expand: HTMLButtonElement | undefined;
    await until(() => {
      expand = [...document.querySelectorAll<HTMLButtonElement>(".menu button")].find((button) => button.textContent?.trim() === "Expand all collapsed");
      return expand?.getAttribute("aria-disabled") === "false";
    }, 3000, "the rail's Expand all collapsed did not enable");
    if (expand!.title !== "Restore full session rows, including sessions hidden by filters") throw new Error("the rail entry has the wrong hint");
    expand!.click();
    await until(() => !document.querySelector(".scard.compact"), 3000, "the rail entry did not expand the row");
    log("background-smoke rail-expand: done");
    log("background-smoke menu: done");
    await exit(0);
  } catch (error) {
    log(`background-smoke failed: ${String(error)}`);
    await exit(1);
  }
}

// `?unread-smoke=1`: an unread reply hidden online appears when its computer goes offline.
async function unreadSmoke(allowed: boolean) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  if (!allowed) {
    log("unread-smoke not started: it needs ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_HGS and ZERUS_CLIENT_PERF");
    return;
  }
  try {
    await until(() => grouping().ready && orderedRows().length > 0, 30_000, "the replay organization was not ready");
    await sleep(500);
    setStateFilter("all");
    const projectUnread = () => {
      const header = listRows().find((row) => row.type === "header" && row.project === defaultProject());
      return header?.type === "header" ? header.unread : -1;
    };
    const computerStatus = (status: string) => applyBatch({ upserts: [], removes: [], replaces: [], computers: [{ computer: "", status, attempt: 1 }], activity: [] });
    const onlineOthers = projectUnread();
    computerStatus("reconnecting");
    await until(() => projectUnread() !== onlineOthers, 5000, "the replay's offline unread replies did not appear");
    const otherDelta = projectUnread() - onlineOthers;
    computerStatus("live");
    await until(() => projectUnread() === onlineOthers, 5000, "the replay did not return online");
    setSearch("offline-unread-smoke");
    const key = "\nrun:offline-unread-smoke";
    applyBatch({
      upserts: [{ computer: "", key: "run:offline-unread-smoke", row: {
        name: "claude/sample-project/offline-unread-smoke", machine: "workstation", agent: "claude",
        state: "running", status: "working", caption: "Working", phase: "working", activity: "busy", process_state: "running",
        tracked: true, created: 1793000000, run_id: "offline-unread-smoke", conversation_id: "conv-offline-unread-smoke",
        reply_id: "reply-offline-unread-smoke", shared_read: true, read_reply_id: null, needs_action: false, needs_attention: false,
        unread: false, unread_candidate: false, summary: "Working",
      } }],
      removes: [], replaces: [], computers: [], activity: [],
    });
    const card = () => document.querySelector<HTMLElement>(`.vrow[data-key="${CSS.escape(key)}"] .scard`);
    await until(() => !!card() && !card()!.querySelector(".sc-extra") && !card()!.hasAttribute("data-emphasis") && projectUnread() >= 0,
      10_000, "the online reply did not stay hidden");
    const before = projectUnread();
    log("unread-smoke online-hidden: done");
    computerStatus("reconnecting");
    await until(() => card()?.dataset.emphasis === "unread" && !!card()?.querySelector(".sc-extra") && projectUnread() === before + otherDelta + 1,
      5000, "the offline card and header did not show the reply").catch(() => {
        const row = orderedRows().find((s) => s.key === key);
        throw new Error(`offline unread state: ${JSON.stringify({ before, otherDelta, after: projectUnread(), status: document.querySelector(".statusbar")?.textContent,
          unread: row?.unread, unreadReply: row?.unreadReply, unreadCandidate: row?.unreadCandidate, card: card()?.outerHTML.slice(0, 600) })}`);
      });
    log("unread-smoke offline-card-header: done");
    const row = document.querySelector<HTMLElement>(`.vrow[data-key="${CSS.escape(key)}"] .rowhit`);
    if (!row) throw new Error("the unread session row is missing");
    row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 320, clientY: 240 }));
    await until(() => !!document.querySelector(".menu"), 3000, "the session menu did not open");
    const compact = [...document.querySelectorAll<HTMLButtonElement>(".menu button")].find((button) => button.textContent?.trim() === "Compact session row");
    if (!compact) throw new Error("Compact session row is missing");
    compact.click();
    await until(() => !!card()?.classList.contains("compact") && card()?.dataset.emphasis === "unread" && !!card()?.querySelector(".sbadge.k-unread"),
      3000, "the compact row did not show the offline reply");
    log("unread-smoke offline-compact: done");
    await exit(0);
  } catch (error) {
    log(`unread-smoke failed: ${String(error)}`);
    await exit(1);
  }
}

const onScenarioStart = (id: string, index: number, total: number) => log(`scenario ${index + 1}/${total} ${id} start`);
const onScenarioEnd = (id: string, index: number, total: number, ms: number, result: ScenarioResult) => {
  finished.push(result);
  log(`scenario ${index + 1}/${total} ${id} end after ${(ms / 1000).toFixed(1)} s${result.skipped ? ` (skipped: ${result.skipped})` : ""}`);
};

export function PerfHost() {
  const urlMode = new URLSearchParams(location.search).get("perf");
  const [config, setConfig] = createSignal<PerfConfig>({ mode: urlMode });
  const [open, setOpen] = createSignal(urlMode === "1" || urlMode === "auto");
  const keys = (e: KeyboardEvent) => {
    if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "d") {
      e.preventDefault();
      setOpen(!open());
    }
  };
  onMount(async () => {
    addEventListener("keydown", keys);
    if (inTauri()) {
      const native = await invoke<PerfConfig>("perf_config").catch(() => undefined);
      if (native) setConfig({ ...native, mode: native.mode ?? urlMode });
    }
    const smoke = new URLSearchParams(location.search).get("actions-smoke");
    if (smoke === "1" && inTauri()) void actionsSmoke(!!config().actions_smoke);
    if (smoke === "marks" && inTauri()) void marksAfterRestart(!!config().actions_smoke);
    if (new URLSearchParams(location.search).get("screens-smoke") === "1" && inTauri()) void screensSmoke(!!config().actions_smoke);
    if (new URLSearchParams(location.search).get("organization-smoke") === "1" && inTauri()) void organizationSmoke(!!config().actions_smoke);
    if (new URLSearchParams(location.search).get("newsession-smoke") === "1" && inTauri()) void newSessionSmoke(!!config().actions_smoke);
    if (new URLSearchParams(location.search).get("phase4-smoke") === "1" && inTauri()) {
      const { phase4Smoke } = await import("../perf/phase4Smoke");
      void phase4Smoke(!!config().actions_smoke, config().smoke_file ?? undefined);
    }
    if (new URLSearchParams(location.search).get("background-smoke") === "1" && inTauri()) void backgroundSmoke(!!config().actions_smoke);
    if (new URLSearchParams(location.search).get("unread-smoke") === "1" && inTauri()) void unreadSmoke(!!config().actions_smoke);
    if (new URLSearchParams(location.search).has("updates-smoke") && inTauri()) {
      const { updatesSmoke } = await import("../perf/updatesSmoke");
      void updatesSmoke(!!config().replay);
    }
    if (new URLSearchParams(location.search).has("window-layer-smoke") && inTauri()) {
      const { windowLayerSmoke } = await import("../perf/windowLayerSmoke");
      void windowLayerSmoke(!!config().replay);
    }
    if (new URLSearchParams(location.search).has("content-search-smoke") && inTauri()) {
      const { contentSearchSmoke } = await import("../perf/contentSearchSmoke");
      void contentSearchSmoke(!!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("project-clone-smoke") && inTauri()) {
      const { projectCloneSmoke } = await import("../perf/projectCloneSmoke");
      void projectCloneSmoke(!!config().replay && !!config().actions_smoke, new URLSearchParams(location.search).get("project-clone-smoke") === "restored");
    }
    if (new URLSearchParams(location.search).has("project-folder-smoke") && inTauri()) {
      const { projectFolderSmoke } = await import("../perf/projectFolderSmoke");
      void projectFolderSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("project-default-smoke") && inTauri()) {
      const { projectDefaultSmoke } = await import("../perf/projectDefaultSmoke");
      void projectDefaultSmoke(!!config().replay && !!config().actions_smoke, new URLSearchParams(location.search).get("project-default-smoke") === "restored");
    }
    if (new URLSearchParams(location.search).has("project-vivid-smoke") && inTauri()) {
      const { projectVividSmoke } = await import("../perf/projectVividSmoke");
      void projectVividSmoke(!!config().replay && !!config().actions_smoke, new URLSearchParams(location.search).get("project-vivid-smoke") === "restored");
    }
    if (new URLSearchParams(location.search).has("project-swarm-ui-smoke") && inTauri()) {
      const { swarmUiSmoke } = await import("../perf/swarmUiSmoke");
      void swarmUiSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("project-swarm-conflicts-smoke") && inTauri()) {
      const { swarmConflictsSmoke } = await import("../perf/swarmConflictsSmoke");
      void swarmConflictsSmoke(!!config().replay && !!config().actions_smoke, new URLSearchParams(location.search).get("project-swarm-conflicts-smoke") === "large");
    }
    if (new URLSearchParams(location.search).has("project-swarm-smoke") && inTauri()) {
      const { swarmSmoke } = await import("../perf/swarmSmoke");
      void swarmSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("worktree-review-smoke") && inTauri()) {
      const { worktreeReviewSmoke } = await import("../perf/worktreeReviewSmoke");
      void worktreeReviewSmoke(!!config().replay && !!config().actions_smoke, ["projects", "overview-native", "overview-ui"].includes(new URLSearchParams(location.search).get("worktree-review-smoke") ?? ""), new URLSearchParams(location.search).get("worktree-review-smoke") === "overview-native", new URLSearchParams(location.search).get("worktree-review-smoke") === "overview-ui");
    }
    if (new URLSearchParams(location.search).has("shell-badge-smoke") && inTauri()) {
      const { shellBadgeSmoke } = await import("../perf/shellBadgeSmoke");
      void shellBadgeSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("termination-cleanup-smoke") && inTauri()) {
      const { terminationCleanupSmoke } = await import("../perf/terminationCleanupSmoke");
      void terminationCleanupSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("archive-cleanup-smoke") && inTauri()) {
      const { archiveCleanupSmoke } = await import("../perf/archiveCleanupSmoke");
      void archiveCleanupSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("end-action-ui-smoke") && inTauri()) {
      const { endActionUiSmoke } = await import("../perf/endActionUiSmoke");
      void endActionUiSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("end-action-native-smoke") && inTauri()) {
      const { endActionNativeSmoke } = await import("../perf/endActionNativeSmoke");
      void endActionNativeSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("file-ssh-smoke") && inTauri()) {
      void import("../perf/fileSshSmoke").then(({ fileSshSmoke }) => fileSshSmoke(!!config().replay && !!config().actions_smoke));
    }
    if (new URLSearchParams(location.search).has("video-poster-smoke") && inTauri()) {
      const { videoPosterSmoke } = await import("../perf/videoPosterSmoke");
      void videoPosterSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("file-reference-smoke") && inTauri()) {
      const { fileReferenceSmoke } = await import("../perf/fileReferenceSmoke");
      void fileReferenceSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("markdown-smoke") && inTauri()) {
      const { markdownSmoke } = await import("../perf/markdownSmoke");
      void markdownSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("machines-setup-smoke") && inTauri()) {
      const { machinesSetupSmoke } = await import("../perf/machinesSmoke");
      void machinesSetupSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("machines-tools-smoke") && inTauri()) {
      const { machinesToolsSmoke } = await import("../perf/machinesSmoke");
      void machinesToolsSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("machines-smoke") && inTauri()) {
      const { machinesSmoke } = await import("../perf/machinesSmoke");
      void machinesSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (new URLSearchParams(location.search).has("native-ui-smoke") && inTauri()) {
      const { nativeUiSmoke } = await import("../perf/nativeUiSmoke");
      void nativeUiSmoke(!!config().replay && !!config().actions_smoke, config().smoke_file ?? undefined);
    }
    if (new URLSearchParams(location.search).has("scope-filter-smoke") && inTauri()) {
      const { scopeFilterSmoke } = await import("../perf/scopeFilterSmoke");
      void scopeFilterSmoke(!!config().replay && !!config().actions_smoke);
    }
    if (config().mode === "auto") {
      setOpen(true);
      void autoRun(config());
    }
  });
  onCleanup(() => removeEventListener("keydown", keys));
  const auto = () => config().mode === "auto";
  return (
    <Show when={open()}>
      <aside class="perf-host" aria-label="Diagnostics">
        <Suspense>
          {/* The unattended run samples resources itself, after its own 10 s idle pause. */}
          <Diagnostics scrollList={scrollList} scrollActivity={scrollActivity} activityEvents={() => activity.events().length} openSession={openSession} displayHz={displayHz}
            metrics={auto() ? undefined : metrics} label={config().label ?? undefined} representative={config().representative}
            onScenarioStart={auto() ? onScenarioStart : undefined} onScenarioEnd={auto() ? onScenarioEnd : undefined} />
        </Suspense>
      </aside>
    </Show>
  );
}
