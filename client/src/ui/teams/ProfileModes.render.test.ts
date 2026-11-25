// @vitest-environment happy-dom
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import type { TeamBuildsView, TeamProfileView, TeamTimelineView, TeamView, TeamTestsView } from "../../types";

// Task #462 (owner, package 168): the plan's Profile view keeps Qt's four modes
// (Time, Tokens, Members, Compare) and the build queue is its own Builds tab.
const NOW = 1_800_000_000;
const span = (task: number, kind: string, from: number, to: number, member = "dev") =>
  ({ task, attempt: 1, kind, stage_kind: "", member, from, to, running: false });

const timeline: TeamTimelineView = {
  team: "t1", phase: 2, from: NOW - 3600, to: NOW, elapsed_s: 3600, worked_s: 1800,
  lanes: [
    { member: "dev", role: "Backend", model: "gpt", utilization: 0.5, busy_s: 1800, idle_s: 600,
      spans: [span(1, "work", NOW - 3000, NOW - 1800), span(1, "rework", NOW - 1700, NOW - 1600)], waits: [span(1, "deps", NOW - 1800, NOW - 900)], idle: [] },
    { member: "qa", role: "Tester", model: "kimi", utilization: 0.2, busy_s: 600, idle_s: 0,
      spans: [span(2, "work", NOW - 900, NOW - 500, "qa")], waits: [], idle: [] },
  ],
  real: [], planned: [], real_s: 0, planned_s: 0, tests: [], tests_more: false,
  estimates: [], titles: { "1": "Saved cards" }, tasks: 2, truncated: false,
};

const profile: TeamProfileView = {
  team: "t1", elapsed_s: 3600, worked_s: 1800, tasks: 4, accepted: 1, tokens: 1200, tokens_partial: false,
  losses: [
    { kind: "rework", task: 1, member: "dev", seconds: 600, tokens: 100, share: 0.2 },
    { kind: "member_wait", task: 2, member: "dev", seconds: 900, tokens: 500, share: 0.3 },
  ],
  roles: [{ key: "Backend", kind: "work", accepted: 1, attempts: 2, returned: 1, tokens_per_accepted: 1200 }],
  task_rows: [], task_rows_more: false,
} as unknown as TeamProfileView;

const team = (): TeamView => ({
  id: "t1", name: "Checkout", status: "running", project: "", folder: "", goal: "", plan_mode: "", lead: "lead",
  revision: 1, created: 0, updated: 0, waiting_for_lead: false, pending_phase: null, settings: { auto_accept: false },
  members: [{ id: "dev", role: "Backend", agent: "codex", lead: false }] as never,
  tasks: [], phases: [
    { n: 1, title: "Foundations", state: "approved", status: "done", done: 2, total: 2, barrier: false, auto: false, acceptance: "", paused: false, health: "" },
    { n: 2, title: "Cards", state: "running", status: "in_work", done: 0, total: 2, barrier: false, auto: false, acceptance: "", paused: false, health: "" },
  ] as never,
  proposals: [], attention: [], validation: { errors: [], warnings: [] }, health: { critical_path: 0, parallelism: 0, warnings: [] },
  ready: [], cycle: [], draft: null, truncated: false,
} as unknown as TeamView);

const builds: TeamBuildsView = {
  team: "t1", enabled: true,
  folders: [{ folder: "D:/Tools/Zerus", members: ["dev", "qa"], build_folders: [{
    name: "cargo", usual_ms: 60000, hit_rate: 0.5,
    running: [{ member: "dev", template: "cargo-build", seconds: 12, usual_ms: 60000, snapshot: true, coalesced_with: "", waiting_for: [], ms: 0, hit: false, by: "", exit: 0, timed_out: false, error: "" }],
    queue: [
      { member: "qa", template: "cargo-test", seconds: 5, usual_ms: 0, snapshot: false, coalesced_with: "", waiting_for: [], ms: 0, hit: false, by: "", exit: 0, timed_out: false, error: "" },
      { member: "dev", template: "cargo-test", seconds: 8, usual_ms: 0, snapshot: false, coalesced_with: "", waiting_for: [], ms: 0, hit: false, by: "", exit: 0, timed_out: false, error: "" },
    ],
    recent: [],
  }] }],
} as unknown as TeamBuildsView;

let dispose: (() => void) | undefined;
const settle = async () => { for (let i = 0; i < 16; i++) await Promise.resolve(); };
let host: HTMLElement;

beforeEach(async () => {
  host = document.createElement("div"); document.body.append(host);
});
afterEach(async () => {
  dispose?.(); dispose = undefined; await settle();
  document.body.replaceChildren(); vi.clearAllMocks();
});

// Task #22 (team ede724c4): the first import of a large module graph is
// transformed inside the 5 s test timeout, which the full client run on a
// loaded machine exceeded ("Test timed out in 5000ms" only under that run).
// It is transformed once here, with its own bound; the tests' own imports then
// resolve from the module cache and test exactly what they did.
beforeAll(async () => { await Promise.all([import("./profile-page"), import("./plan-panels")]); }, 60_000);

async function mountProfile() {
  const { ProfilePage } = await import("./profile-page");
  dispose = render(() => createComponent(ProfilePage, {
    team: team(), profile, loadTimeline: async () => timeline, statusName: (s: string) => s,
  }), host);
  await settle();
}
const click = (selector: string) => {
  const element = host.querySelector<HTMLElement>(selector);
  expect(element, selector).not.toBeNull();
  element!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
};

describe("the plan's Profile modes (task #462)", () => {
  it("shows Qt's four modes as one segmented switch, Time first", async () => {
    await mountProfile();
    const tabs = [...host.querySelectorAll<HTMLElement>("[data-profile-mode]")].map((tab) => tab.dataset.profileMode);
    expect(tabs).toEqual(["time", "tokens", "members", "compare"]);
    expect([...host.querySelectorAll("[data-profile-mode]")].map((tab) => tab.textContent)).toEqual(["Time", "Tokens", "Members", "Compare"]);
    // Time is the page it opens on: the totals and the losses order.
    expect(host.querySelector("[data-loss-order]")).not.toBeNull();
    expect(host.querySelector('[data-profile-mode="time"]')!.getAttribute("aria-selected")).toBe("true");
  });

  it("keeps the losses order switch and the Top losses after the lanes (Qt TeamProfile.cpp:917)", async () => {
    await mountProfile();
    expect([...host.querySelectorAll("[data-loss-order] button")].map((button) => button.textContent)).toEqual(["By time", "By tokens"]);
    click('[data-loss-order] button:nth-child(2)');
    await settle();
    const rows = [...host.querySelectorAll(".pm-loss-row")].map((row) => row.textContent ?? "");
    // By tokens: the 500-token member wait comes before the 100-token rework.
    expect(rows[0]).toContain("member");
    expect(rows[0]).toContain("500");
  });

  it("draws the Tokens page from the profile read", async () => {
    await mountProfile();
    click('[data-profile-mode="tokens"]');
    await settle();
    expect(host.querySelector('[data-profile-page="tokens"]')).not.toBeNull();
    const roles = [...host.querySelectorAll("[data-profile-roles] tbody tr")];
    expect(roles).toHaveLength(1);
    expect(roles[0].textContent).toContain("Backend");
    expect(host.querySelector("[data-profile-token-losses]")).not.toBeNull();
    // The tokens total, the format the client uses for big numbers.
    expect(host.querySelector("[data-profile-tokens-total]")!.textContent).toContain("1.2k");
  });

  it("draws the Members table from the timeline lanes", async () => {
    await mountProfile();
    click('[data-profile-mode="members"]');
    await settle();
    const rows = [...host.querySelectorAll<HTMLElement>("[data-profile-members] tbody tr")].map((row) => row.dataset.member);
    expect(rows).toEqual(["dev", "qa"]);
    const dev = host.querySelector('[data-member="dev"]')!;
    expect(dev.textContent).toContain("gpt");
    // attempts: one work and one rework of task 1
    expect(dev.textContent).toContain("2");
    // work redone: 100s of rework against 1200s of work
    expect(dev.textContent).toContain("8%");
    // the profile's per-role numbers: the Backend role's tokens per accepted
    expect(dev.textContent).toContain("1.2k");
  });

  it("compares two phases by their timeline reads", async () => {
    await mountProfile();
    click('[data-profile-mode="compare"]');
    await settle();
    expect(host.querySelector("[data-profile-compare]")).not.toBeNull();
    const measures = [...host.querySelectorAll<HTMLElement>("[data-measure]")].map((row) => row.dataset.measure);
    expect(measures).toEqual(["took", "tasks", "worked", "rework", "waited", "tests"]);
    expect(host.querySelectorAll("[data-compare-a], [data-compare-b]")).toHaveLength(2);
  });
});

describe("the Builds tab (task #462)", () => {
  it("labels executor capacity and activity separately and refuses missing load values", async () => {
    const { TestsPanel } = await import("./plan-panels");
    const tests: TeamTestsView = { team: "t1", queue: [], next: null, runs: [{ id: "remote-perf", suite: "smoke", status: "passed", asked_by: "qa", task: 1, at: 1, duration_ms: 100, recorded_ms: 100, runner: "executor:stand", excerpt: "", flags: ["loaded"], reused_from: "", due: false, over_budget: false, executor_load: { valid: true, loaded: true, sample_kind: "during_peak", cpu_percent: 70, disk_percent: 80, disk_busy_percent: 20, waited_ms: 2000 } }, { id: "no-measurement", suite: "unavailable", status: "not_run", asked_by: "qa", task: 1, at: 2, duration_ms: 0, recorded_ms: 0, runner: "executor:stand", excerpt: "", flags: [], reused_from: "", due: false, over_budget: false, executor_load: { valid: false, loaded: false, sample_kind: "before", cpu_percent: null, disk_percent: null, disk_busy_percent: null, waited_ms: 0 } }] };
    dispose = render(() => createComponent(TestsPanel, { tests }), host); await settle();
    const measured = host.querySelector('[data-executor-load="remote-perf"]')!.textContent;
    expect(measured).toContain("Disk busy 20%"); expect(measured).toContain("Disk used 80%");
    expect(host.querySelector('[data-executor-load="no-measurement"]')!.textContent).toContain("unavailable");
    expect(host.querySelector('[data-executor-load="no-measurement"]')!.textContent).not.toContain("CPU 0%");
  });
  it("shows remote devices and unresolved execution without claiming success", async () => {
    const { BuildsPanel } = await import("./plan-panels");
    const remote = { ...builds, deliveries: [{ job: "synthetic-job", profile: "weak-stand", executor: "synthetic-executor", device: "host", phase: "unknown", updated: 1 }] };
    dispose = render(() => createComponent(BuildsPanel, { builds: remote }), host);
    await settle();
    expect(host.querySelector("[data-artifact-deliveries]")!.textContent).toContain("weak-stand");
    expect(host.querySelector('[data-artifact-job="synthetic-job"]')!.textContent).toContain("unknown");
    expect(host.textContent).toContain("review before starting a new job");
  });
  it("is its own plan mode beside Tests", async () => {
    const { PlanModeBar } = await import("./plan-panels");
    dispose = render(() => createComponent(PlanModeBar, { mode: "tests", onMode: () => {} }), host);
    await settle();
    const modes = [...host.querySelectorAll<HTMLElement>("[data-mode]")].map((tab) => tab.dataset.mode);
    expect(modes).toEqual(["graph", "board", "layers", "lanes", "profile", "tests", "builds"]);
    expect(host.querySelector('[data-mode="builds"]')!.textContent).toContain("Builds");
  });

  it("counts what builds and waits, per folder and build folder (Qt TeamTests.cpp:674-690)", async () => {
    const { BuildsPanel } = await import("./plan-panels");
    dispose = render(() => createComponent(BuildsPanel, { builds }), host);
    await settle();
    expect(host.querySelector('[data-count="building"]')!.textContent).toBe("1 building");
    expect(host.querySelector('[data-count="waiting"]')!.textContent).toBe("2 waiting");
    expect(host.querySelector('[data-folder="D:/Tools/Zerus"]')).not.toBeNull();
    expect(host.querySelector('[data-build-folder="cargo"]')).not.toBeNull();
    // the running member's line and the two queued ones
    expect(host.querySelectorAll(".pm-build-running")).toHaveLength(1);
    expect(host.querySelectorAll(".pm-build-queue")).toHaveLength(2);
  });
});
