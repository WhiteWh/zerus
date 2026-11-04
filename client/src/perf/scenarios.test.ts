import { describe, expect, it, vi } from "vitest";
import {
  ACTIVITY_CANDIDATE_CAP,
  DEFAULT_PLAN,
  SCENARIOS,
  activityCandidates,
  activitySource,
  runScenario,
  summarize,
  toJson,
  type DiagnosticsTargets,
  type PerfReport,
  type Plan,
} from "./scenarios";

describe("summarize", () => {
  it("gives the count, mean, median, nearest-rank percentiles and max", () => {
    const s = summarize([5, 1, 4, 2, 3]);
    expect(s).toEqual({ count: 5, mean: 3, median: 3, p95: 5, p99: 5, max: 5 });
    expect(summarize([...Array(100).keys()].map((i) => i + 1)).p95).toBe(95);
  });

  it("is NaN for no samples", () => {
    const s = summarize([]);
    expect(s.count).toBe(0);
    expect(Number.isNaN(s.p95)).toBe(true);
  });
});

describe("report JSON", () => {
  it("writes NaN and infinities as null", () => {
    const report = { schema: "zerus-perf-v1", scenarios: [{ worst: { mean: NaN, max: Infinity, p95: 1.5 } }] } as unknown as PerfReport;
    expect(JSON.parse(toJson(report)).scenarios[0].worst).toEqual({ mean: null, max: null, p95: 1.5 });
  });

  it("covers every scenario of the spec", () => {
    expect(SCENARIOS.map((s) => s.id)).toEqual([
      "raf-light",
      "transform-animation",
      "left-animation",
      "list-scroll",
      "activity-scroll",
      "session-open",
      "input-latency",
    ]);
  });
});

describe("scroll scenario skips", () => {
  const plan: Plan = { ...DEFAULT_PLAN, hz: 60, hzSource: "manual" };
  const box = (scrollHeight: number, clientHeight: number) => ({ scrollHeight, clientHeight, scrollTop: 0 }) as unknown as HTMLElement;
  const targets = (list: HTMLElement | null, activity: HTMLElement | null): DiagnosticsTargets => ({
    scrollList: () => list,
    scrollActivity: () => activity,
    openSession: async () => {},
  });
  const stage = null as never;

  it("says how far the Activity scrolled when there is nothing to scroll", async () => {
    const result = await runScenario("activity-scroll", targets(null, box(512, 500)), stage, plan);
    expect(result.skipped).toBe("nothing to scroll (12 px)");
  });

  it("says how far the list scrolled, and 0 px for a list that fits", async () => {
    expect((await runScenario("list-scroll", targets(box(500, 500), null), stage, plan)).skipped).toBe("nothing to scroll (0 px)");
    expect((await runScenario("list-scroll", targets(box(563, 500), null), stage, plan)).skipped).toBe("nothing to scroll (63 px)");
  });

  it("keeps the missing container reason", async () => {
    expect((await runScenario("activity-scroll", targets(null, null), stage, plan)).skipped).toBe("no scroll container");
  });
});

describe("Activity content growth diagnostics", () => {
  it("records counts around every run without adding per-frame geometry reads", async () => {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      queueMicrotask(() => callback(performance.now()));
      return 1;
    });
    const queries = vi.fn((selector: string) => ({ length: selector === '[id^="item-"]' ? 7 : 11 }));
    const activity = {
      scrollHeight: 1500, clientHeight: 500, scrollTop: 0,
      addEventListener: () => {}, removeEventListener: () => {}, querySelectorAll: queries,
    } as unknown as HTMLElement;
    const events = vi.fn().mockReturnValueOnce(100).mockReturnValueOnce(200).mockReturnValueOnce(300).mockReturnValueOnce(400);
    const targets: DiagnosticsTargets = { scrollList: () => null, scrollActivity: () => activity, activityEvents: events, openSession: async () => {} };
    try {
      const plan: Plan = { ...DEFAULT_PLAN, hz: 60, hzSource: "manual", warmupMs: 0, runMs: 0, runs: 1 };
      const result = await runScenario("activity-scroll", targets, null as never, plan);
      expect(result.runs[0]).toMatchObject({
        activity_before: { scroll_height: 1500, activity_rows: 7, dom_nodes: 11, events: 300 },
        activity_after: { scroll_height: 1500, activity_rows: 7, dom_nodes: 11, events: 400 },
      });
      expect(events).toHaveBeenCalledTimes(4);
      expect(queries).toHaveBeenCalledTimes(8);
    } finally { vi.unstubAllGlobals(); }
  });
});

describe("Activity candidates", () => {
  const cards = Array.from({ length: 40 }, (_, i) => `card-${i}`);

  it("caps a live run at 16 with the chosen session first", () => {
    const live = activityCandidates("card-30", cards, false);
    expect(live).toHaveLength(ACTIVITY_CANDIDATE_CAP);
    expect(live[0]).toBe("card-30");
  });

  it("tries every card of a replay run, whatever the list order", () => {
    expect(activityCandidates(undefined, cards, true)).toEqual(cards);
    expect(activityCandidates(undefined, [...cards].reverse(), true)).toHaveLength(40);
    expect(activityCandidates("card-3", cards, true)).toHaveLength(40);
  });
});

describe("activity source in the report", () => {
  it("holds counts and the replay flag only", () => {
    const source = activitySource(true, { events: 1100, range: 2412.5, pages: 4 });
    expect(source).toEqual({ replay: true, events: 1100, scroll_px: 2412, older_pages: 4 });
  });

  it("is empty-handed but present when no session had events", () => {
    expect(activitySource(false, undefined)).toEqual({ replay: false, events: 0, scroll_px: 0, older_pages: 0 });
  });
});
