// Scripted measurement scenarios for the client (zerus-5wmh.4).
//
// Each scenario runs a warm-up (2 s by default) and then three timed runs
// (10 s each); the worst run counts, as the spec's macOS criteria require.
// Results are plain JSON (schema "zerus-perf-v1"), the same shape the Qt
// harness in client/bench/qt writes, so client/scripts/measure-report.py can
// put both on one page. No session content is recorded: only timings.

import { frameBudget, passes, recordFrames, stats, worstRun, type PacingStats, type PacingVerdict } from "./pacing";

/** What the app gives the diagnostics: its two scroll containers and a way to open a session. */
export interface DiagnosticsTargets {
  scrollList: () => HTMLElement | null;
  scrollActivity: () => HTMLElement | null;
  /** Only counts are recorded, so retained content growth is visible across runs. */
  activityEvents?: () => number;
  /** Opens the session at `index` of the visible list and resolves once its Activity is in the DOM. */
  openSession: (index: number) => Promise<void>;
}

export interface Plan {
  /** Display refresh rate; T = 1000 / hz. */
  hz: number;
  hzSource: "native" | "manual" | "estimated";
  warmupMs: number;
  runMs: number;
  runs: number;
  /** Scroll distance per frame for the scroll scenarios. */
  stepPx: number;
  /** Most opens per run of the session-open scenario. */
  opens: number;
  /** List indices the session-open scenario alternates between. */
  openIndices: number[];
  /** Synthetic pointer events (false: count only real clicks on the probe). */
  syntheticInput: boolean;
}

export const DEFAULT_PLAN: Omit<Plan, "hz" | "hzSource"> = {
  warmupMs: 2000,
  runMs: 10000,
  runs: 3,
  stepPx: 16,
  opens: 20,
  openIndices: [0, 1],
  syntheticInput: true,
};

export type ScenarioId =
  | "raf-light"
  | "transform-animation"
  | "left-animation"
  | "list-scroll"
  | "activity-scroll"
  | "session-open"
  | "input-latency";

export const SCENARIOS: { id: ScenarioId; label: string }[] = [
  { id: "raf-light", label: "rAF on a light page" },
  { id: "transform-animation", label: "rAF during a CSS transform animation" },
  { id: "left-animation", label: "rAF during a non-accelerated animation (left)" },
  { id: "list-scroll", label: "Session list scroll" },
  { id: "activity-scroll", label: "Activity scroll" },
  { id: "session-open", label: "Session open to first Activity paint" },
  { id: "input-latency", label: "Input to frame latency" },
];

export interface Summary {
  count: number;
  mean: number;
  median: number;
  p95: number;
  p99: number;
  max: number;
}

export interface ActivityContent {
  scroll_height: number;
  activity_rows: number;
  dom_nodes: number;
  events: number;
}

export interface PacingRun {
  frames: number;
  interval_ms: PacingStats & { max: number };
  verdict: PacingVerdict;
  /** Intervals between scroll events (scroll scenarios). */
  scroll_event_ms?: Summary;
  activity_before?: ActivityContent;
  activity_after?: ActivityContent;
}

export interface TimingRun {
  count: number;
  /** session-open: open to first paint; input-latency: input to the next frame. */
  wall_ms?: Summary;
  latency_ms?: Summary;
  /** input-latency: input to the task after that frame's paint. */
  to_paint_ms?: Summary;
}

export interface ScenarioResult {
  id: ScenarioId;
  label: string;
  kind: "pacing" | "timing" | "latency";
  metric: "interval_ms" | "wall_ms" | "latency_ms";
  runs: (PacingRun | TimingRun)[];
  worst_index: number;
  worst: PacingRun | TimingRun | null;
  /** Criterion 1 for the worst run of a pacing scenario. */
  verdict?: PacingVerdict;
  skipped?: string;
}

export interface PerfReport {
  schema: "zerus-perf-v1";
  client: "tauri";
  created_at: string;
  platform: {
    os: string;
    label: string;
    user_agent: string;
    hz: number;
    hz_source: Plan["hzSource"];
    T_ms: number;
    dpr: number;
    cross_origin_isolated: boolean;
    timer_resolution_ms: number;
    reduced_motion: boolean;
    representative: boolean;
  };
  settings: Omit<Plan, "hz" | "hzSource">;
  scenarios: ScenarioResult[];
  resources?: Record<string, unknown>;
  /** Where the Activity scenario's session came from (unattended runs); counts only. */
  activity_source?: ActivitySource;
}

/** Which Activity the run measured: a replay or the live catalog, its size, scroll range and older pages read. Counts only, no names. */
export interface ActivitySource {
  replay: boolean;
  events: number;
  scroll_px: number;
  older_pages: number;
}

/** The most sessions a live run tries for an Activity that scrolls. */
export const ACTIVITY_CANDIDATE_CAP = 16;

/**
 * The sessions an unattended run tries for its Activity, a chosen one first.
 * A live run stops at the cap; a replay run tries every card, so the long
 * fixture session is found whatever the list order.
 */
export function activityCandidates(selected: string | undefined, cards: readonly string[], replay: boolean): string[] {
  const all = [...new Set([selected, ...cards].filter((id): id is string => !!id))];
  return replay ? all : all.slice(0, ACTIVITY_CANDIDATE_CAP);
}

export function activitySource(replay: boolean, best: { events: number; range: number; pages: number } | undefined): ActivitySource {
  return { replay, events: best?.events ?? 0, scroll_px: Math.max(0, Math.floor(best?.range ?? 0)), older_pages: best?.pages ?? 0 };
}

export function summarize(values: readonly number[]): Summary {
  const sorted = [...values].sort((a, b) => a - b);
  const n = sorted.length;
  if (n === 0) return { count: 0, mean: NaN, median: NaN, p95: NaN, p99: NaN, max: NaN };
  const rank = (p: number) => sorted[Math.min(n, Math.max(1, Math.ceil((p / 100) * n))) - 1];
  const middle = n >> 1;
  return {
    count: n,
    mean: sorted.reduce((a, b) => a + b, 0) / n,
    median: n % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2,
    p95: rank(95),
    p99: rank(99),
    max: sorted[n - 1],
  };
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
const frame = () => new Promise<number>((resolve) => requestAnimationFrame(() => resolve(performance.now())));

/** Resolves in the task after the next frame's rendering, an approximation of "painted". */
export function afterNextPaint(): Promise<number> {
  return new Promise((resolve) =>
    requestAnimationFrame(() => {
      const channel = new MessageChannel();
      channel.port1.onmessage = () => {
        channel.port1.close();
        resolve(performance.now());
      };
      channel.port2.postMessage(0);
    }),
  );
}

/** The smallest non-zero step of performance.now(): coarse timers make pacing figures meaningless. */
export function timerResolution(samples = 5000): number {
  let best = Infinity;
  let last = performance.now();
  for (let i = 0; i < samples; i++) {
    const now = performance.now();
    if (now > last) best = Math.min(best, now - last);
    last = now;
  }
  return best;
}

const COMMON_RATES = [60, 75, 90, 100, 120, 144, 165, 240];

/**
 * Display rate estimated from rAF for when the native side cannot tell. On macOS
 * this reads 60 when WebKit caps rendering, which is what the measurement tests:
 * never use the estimate for the macOS verdict.
 */
export async function estimateHz(ms = 1000): Promise<number> {
  const deltas = await recordFrames(ms);
  const median = summarize(deltas).median;
  const hz = 1000 / median;
  return COMMON_RATES.reduce((best, rate) => (Math.abs(rate - hz) < Math.abs(best - hz) ? rate : best), 60);
}

/** A small stage inside the diagnostics panel where animations and the input probe live. */
export interface Stage {
  root: HTMLElement;
}

const STYLE_ID = "zerus-perf-style";

function ensureStyle() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  // The measurement animates on purpose, also when reduced motion is asked for.
  style.textContent = `
@keyframes zerus-perf-transform { from { transform: translateX(0) } to { transform: translateX(160px) } }
@keyframes zerus-perf-left { from { left: 0 } to { left: 160px } }
.zerus-perf-box { position: absolute; top: 4px; left: 0; width: 32px; height: 24px; border-radius: 4px;
  background: var(--z-color-accent, #4c8dff); }
.zerus-perf-box.transform { animation: zerus-perf-transform 1s linear infinite alternate; will-change: transform; }
.zerus-perf-box.left { animation: zerus-perf-left 1s linear infinite alternate; }
`;
  document.head.appendChild(style);
}

function withBox<T>(stage: Stage, kind: "transform" | "left", body: () => Promise<T>): Promise<T> {
  ensureStyle();
  const box = document.createElement("div");
  box.className = `zerus-perf-box ${kind}`;
  stage.root.appendChild(box);
  return body().finally(() => box.remove());
}

async function pacingRuns(
  plan: Plan,
  start: () => (() => void) | void,
  scrollTarget?: HTMLElement,
  activityContent?: () => ActivityContent,
): Promise<PacingRun[]> {
  const T = frameBudget(plan.hz);
  const once = async (ms: number): Promise<PacingRun> => {
    const before = activityContent?.();
    const scrollTimes: number[] = [];
    const onScroll = () => scrollTimes.push(performance.now());
    scrollTarget?.addEventListener("scroll", onScroll, { passive: true });
    const stop = start();
    let deltas: number[];
    try {
      deltas = await recordFrames(ms, {
        onFrame: scrollTarget ? () => advance(scrollTarget, plan.stepPx) : undefined,
      });
    } finally {
      stop?.();
      scrollTarget?.removeEventListener("scroll", onScroll);
    }
    const after = activityContent?.();
    const s = stats(deltas, T);
    const run: PacingRun = {
      frames: deltas.length,
      interval_ms: { ...s, max: deltas.length ? Math.max(...deltas) : NaN },
      verdict: passes(s, T),
    };
    if (scrollTarget) run.scroll_event_ms = summarize(scrollTimes.slice(1).map((t, i) => t - scrollTimes[i]));
    if (before && after) { run.activity_before = before; run.activity_after = after; }
    return run;
  };
  await once(plan.warmupMs);
  const runs: PacingRun[] = [];
  for (let i = 0; i < plan.runs; i++) runs.push(await once(plan.runMs));
  return runs;
}

// Scroll direction per element, so a run bounces between the ends instead of jumping back.
const directions = new WeakMap<HTMLElement, number>();

function advance(element: HTMLElement, step: number) {
  const direction = directions.get(element) ?? 1;
  const max = element.scrollHeight - element.clientHeight;
  let next = element.scrollTop + direction * step;
  if (next >= max || next <= 0) {
    directions.set(element, -direction);
    next = Math.min(Math.max(next, 0), max);
  }
  element.scrollTop = next;
}

function pacingResult(id: ScenarioId, plan: Plan, runs: PacingRun[]): ScenarioResult {
  const T = frameBudget(plan.hz);
  const index = worstRun(
    runs.map((r) => r.interval_ms),
    T,
  );
  return {
    id,
    label: labelOf(id),
    kind: "pacing",
    metric: "interval_ms",
    runs,
    worst_index: index,
    worst: runs[index] ?? null,
    verdict: runs[index]?.verdict,
  };
}

function worstBy(runs: TimingRun[], metric: "wall_ms" | "latency_ms"): number {
  let worst = 0;
  runs.forEach((run, i) => {
    if ((run[metric]?.p95 ?? -1) > (runs[worst][metric]?.p95 ?? -1)) worst = i;
  });
  return worst;
}

function labelOf(id: ScenarioId) {
  return SCENARIOS.find((s) => s.id === id)?.label ?? id;
}

function skipped(id: ScenarioId, kind: ScenarioResult["kind"], metric: ScenarioResult["metric"], why: string): ScenarioResult {
  return { id, label: labelOf(id), kind, metric, runs: [], worst_index: -1, worst: null, skipped: why };
}

async function sessionOpen(targets: DiagnosticsTargets, plan: Plan): Promise<ScenarioResult> {
  const once = async (ms: number): Promise<TimingRun> => {
    const times: number[] = [];
    const started = performance.now();
    let i = 0;
    while (performance.now() - started < ms && times.length < plan.opens) {
      const index = plan.openIndices[i++ % plan.openIndices.length];
      const t0 = performance.now();
      await targets.openSession(index);
      const painted = await afterNextPaint();
      times.push(painted - t0);
    }
    return { count: times.length, wall_ms: summarize(times) };
  };
  try {
    await once(plan.warmupMs);
    const runs: TimingRun[] = [];
    for (let r = 0; r < plan.runs; r++) runs.push(await once(plan.runMs));
    const index = worstBy(runs, "wall_ms");
    return { id: "session-open", label: labelOf("session-open"), kind: "timing", metric: "wall_ms", runs, worst_index: index, worst: runs[index] };
  } catch (error) {
    return skipped("session-open", "timing", "wall_ms", String(error));
  }
}

async function inputLatency(stage: Stage, plan: Plan): Promise<ScenarioResult> {
  ensureStyle();
  const probe = document.createElement("button");
  probe.type = "button";
  probe.textContent = "0";
  probe.setAttribute("aria-label", "Input latency probe");
  probe.style.cssText = "position:absolute; right:4px; top:2px; min-width:48px;";
  stage.root.appendChild(probe);
  let presses = 0;
  let dispatchedAt = -1;
  let toFrame: number[] = [];
  let toPaint: number[] = [];
  const onDown = (event: PointerEvent) => {
    // A real event carries the platform's input time; a synthetic one the dispatch time.
    const at = event.isTrusted ? event.timeStamp : dispatchedAt >= 0 ? dispatchedAt : event.timeStamp;
    if (!plan.syntheticInput && !event.isTrusted) return;
    probe.textContent = String(++presses);
    probe.classList.toggle("pressed");
    requestAnimationFrame(() => {
      toFrame.push(performance.now() - at);
      const channel = new MessageChannel();
      channel.port1.onmessage = () => {
        channel.port1.close();
        toPaint.push(performance.now() - at);
      };
      channel.port2.postMessage(0);
    });
  };
  probe.addEventListener("pointerdown", onDown);
  const once = async (ms: number): Promise<TimingRun> => {
    toFrame = [];
    toPaint = [];
    const started = performance.now();
    while (performance.now() - started < ms) {
      // A random phase relative to the frame, like real input.
      await sleep(40 + Math.random() * 70);
      if (plan.syntheticInput) {
        dispatchedAt = performance.now();
        probe.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, pointerType: "mouse", isPrimary: true }));
        dispatchedAt = -1;
      }
    }
    await frame();
    await afterNextPaint();
    return { count: toFrame.length, latency_ms: summarize(toFrame), to_paint_ms: summarize(toPaint) };
  };
  try {
    await once(plan.warmupMs);
    const runs: TimingRun[] = [];
    for (let r = 0; r < plan.runs; r++) runs.push(await once(plan.runMs));
    const index = worstBy(runs, "latency_ms");
    return {
      id: "input-latency",
      label: labelOf("input-latency"),
      kind: "latency",
      metric: "latency_ms",
      runs,
      worst_index: index,
      worst: runs[index],
    };
  } finally {
    probe.removeEventListener("pointerdown", onDown);
    probe.remove();
  }
}

export async function runScenario(id: ScenarioId, targets: DiagnosticsTargets, stage: Stage, plan: Plan): Promise<ScenarioResult> {
  switch (id) {
    case "raf-light":
      return pacingResult(id, plan, await pacingRuns(plan, () => undefined));
    case "transform-animation":
      return withBox(stage, "transform", async () => pacingResult(id, plan, await pacingRuns(plan, () => undefined)));
    case "left-animation":
      return withBox(stage, "left", async () => pacingResult(id, plan, await pacingRuns(plan, () => undefined)));
    case "list-scroll":
    case "activity-scroll": {
      const element = id === "list-scroll" ? targets.scrollList() : targets.scrollActivity();
      if (!element) return skipped(id, "pacing", "interval_ms", "no scroll container");
      const range = element.scrollHeight - element.clientHeight;
      if (range < plan.stepPx * 4) return skipped(id, "pacing", "interval_ms", `nothing to scroll (${Math.max(0, Math.floor(range))} px)`);
      const top = element.scrollTop;
      try {
        const content = id === "activity-scroll" ? (): ActivityContent => ({
          scroll_height: element.scrollHeight,
          activity_rows: element.querySelectorAll('[id^="item-"]').length,
          dom_nodes: element.querySelectorAll("*").length,
          events: targets.activityEvents?.() ?? -1,
        }) : undefined;
        return pacingResult(id, plan, await pacingRuns(plan, () => undefined, element, content));
      } finally {
        element.scrollTop = top;
      }
    }
    case "session-open":
      return sessionOpen(targets, plan);
    case "input-latency":
      return inputLatency(stage, plan);
  }
}

export interface RunAllOptions {
  ids?: ScenarioId[];
  label?: string;
  os?: string;
  representative?: boolean;
  onProgress?: (id: ScenarioId, index: number, total: number) => void;
  /** Resource sample (the Tauri `metrics` command) taken after an idle pause. */
  metrics?: () => Promise<unknown>;
  idleMs?: number;
  /** Called when a scenario ends, with its wall-clock time and result. */
  onScenarioEnd?: (id: ScenarioId, index: number, total: number, ms: number, result: ScenarioResult) => void;
  /** A scenario that runs longer ends as skipped "no frames" (rAF stops while a window is hidden or occluded). */
  scenarioTimeoutMs?: number;
}

/** The longest one scenario may take: three 10 s runs, a warm-up and its pauses fit well within it. */
export const SCENARIO_TIMEOUT_MS = 60_000;

const KIND: Record<ScenarioId, [ScenarioResult["kind"], ScenarioResult["metric"]]> = {
  "raf-light": ["pacing", "interval_ms"],
  "transform-animation": ["pacing", "interval_ms"],
  "left-animation": ["pacing", "interval_ms"],
  "list-scroll": ["pacing", "interval_ms"],
  "activity-scroll": ["pacing", "interval_ms"],
  "session-open": ["timing", "wall_ms"],
  "input-latency": ["latency", "latency_ms"],
};

/** Runs one scenario, or gives up after `ms` (the abandoned run never resolves while no frames come). */
function bounded(id: ScenarioId, run: Promise<ScenarioResult>, ms: number): Promise<ScenarioResult> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<ScenarioResult>((resolve) => {
    timer = setTimeout(() => resolve(skipped(id, KIND[id][0], KIND[id][1], "no frames")), ms);
  });
  return Promise.race([run, timeout]).finally(() => clearTimeout(timer));
}

export async function runAll(targets: DiagnosticsTargets, stage: Stage, plan: Plan, options: RunAllOptions = {}): Promise<PerfReport> {
  const ids = options.ids ?? SCENARIOS.map((s) => s.id);
  const scenarios: ScenarioResult[] = [];
  for (const [index, id] of ids.entries()) {
    options.onProgress?.(id, index, ids.length);
    const started = performance.now();
    const result = await bounded(id, runScenario(id, targets, stage, plan), options.scenarioTimeoutMs ?? SCENARIO_TIMEOUT_MS);
    scenarios.push(result);
    options.onScenarioEnd?.(id, index, ids.length, performance.now() - started, result);
  }
  const report: PerfReport = {
    schema: "zerus-perf-v1",
    client: "tauri",
    created_at: new Date().toISOString(),
    platform: platformInfo(plan, options),
    settings: {
      warmupMs: plan.warmupMs,
      runMs: plan.runMs,
      runs: plan.runs,
      stepPx: plan.stepPx,
      opens: plan.opens,
      openIndices: plan.openIndices,
      syntheticInput: plan.syntheticInput,
    },
    scenarios,
  };
  if (options.metrics) {
    await sleep(options.idleMs ?? 5000);
    try {
      report.resources = { idle: await options.metrics() };
    } catch (error) {
      report.resources = { idle: { error: String(error) } };
    }
  }
  return report;
}

export function platformInfo(plan: Plan, options: RunAllOptions = {}): PerfReport["platform"] {
  const ua = navigator.userAgent;
  const os = options.os ?? (/Mac OS X/.test(ua) ? "macos" : /Windows/.test(ua) ? "windows" : /Linux/.test(ua) ? "linux" : "unknown");
  return {
    os,
    label: options.label ?? os,
    user_agent: ua,
    hz: plan.hz,
    hz_source: plan.hzSource,
    T_ms: frameBudget(plan.hz),
    dpr: window.devicePixelRatio,
    cross_origin_isolated: Boolean((globalThis as { crossOriginIsolated?: boolean }).crossOriginIsolated),
    timer_resolution_ms: timerResolution(),
    reduced_motion: matchMedia("(prefers-reduced-motion: reduce)").matches,
    representative: options.representative ?? true,
  };
}

/** The report as a pretty JSON string; NaN becomes null. */
export function toJson(report: PerfReport): string {
  return JSON.stringify(report, (_key, value) => (typeof value === "number" && !Number.isFinite(value) ? null : value), 2);
}

/** Offers the report as a file download from the page. */
export function download(report: PerfReport, name?: string) {
  const blob = new Blob([toJson(report)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  const stamp = report.created_at.replace(/[:]/g, "-").replace(/\.\d+Z$/, "Z");
  link.download = name ?? `zerus-perf-${report.platform.label}-${stamp}.json`;
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
