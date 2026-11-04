import { describe, expect, it } from "vitest";
import { createRoot } from "solid-js";
import { createQuestions, type Clock, type QuestionPort } from "./questions";
import type { ButtonView, CardView, QuestionView } from "../types";

const button = (over: Partial<ButtonView> = {}): ButtonView => ({ visible: true, enabled: true, label: { source: "Submit answer" }, review_label: null, ...over });

function card(over: Partial<CardView> = {}, revision = 1, epoch = revision): QuestionView {
  return {
    card: "question\n[\"studio\\nclaude/sample-project/review\",\"tool:t1\",\"h\"]\nrun-1\nconv-1", revision, epoch, asked: "", asked_tip: "", terminal_label: { source: "Open Terminal" }, native_ui: false,
    view: {
      heading: { source: "Agent needs your answer" }, progress: null, pending: { source: "%1 pending", args: [{ kind: "number", value: 1 }] }, pending_tip: { source: "" }, nav: null, tabs_visible: false, page: 0, forms: [],
      status: { source: "" }, status_error: false, available: true, locked: false, review: false, created_at: 0,
      submit: button(), approve: button({ visible: false, label: { source: "Approve once" } }), deny: button({ visible: false, label: { source: "Deny" } }),
      skip: button({ visible: false, label: { source: "Skip" } }), previous: button({ visible: false, label: { source: "← Back" } }), next: button({ visible: false, label: { source: "Next →" } }),
      retry: false, skip_tip: { source: "" }, retry_tip: { source: "" }, ...over,
    },
  };
}

const approval = (revision = 1) => card({ heading: { source: "Agent needs your approval" }, review: true, submit: button({ visible: false }),
  approve: button({ label: { source: "Approve once" }, review_label: { source: "Approve once" } }), deny: button({ label: { source: "Deny" } }) }, revision);

function fakes() {
  const calls: string[] = [];
  let now = 0;
  const ticks = new Set<() => void>();
  const clock: Clock = { now: () => now, every: (_ms, f) => { ticks.add(f); return () => ticks.delete(f); } };
  let next: Record<string, unknown> = { kind: "sending" };
  const port: QuestionPort = {
    edit: async (_c, epoch, e) => { calls.push(`edit ${epoch} ${JSON.stringify(e)}`); },
    page: async (_c, p) => { calls.push(`page ${p}`); },
    navigate: async (d) => { calls.push(`navigate ${d}`); },
    allowRetry: async () => { calls.push("retry"); },
    submit: async () => { calls.push("submit"); return next as never; },
    decide: async (_c, allow) => { calls.push(`decide ${allow}`); return next as never; },
    skip: async () => { calls.push("skip"); return next as never; },
    cold: async (token, proceed) => { calls.push(`cold ${token} ${proceed}`); return { kind: "busy" }; },
    notify: (text) => { calls.push(`notify ${text}`); },
  };
  return { calls, clock, port, ticking: () => ticks.size, advance(ms: number) { now += ms; ticks.forEach((f) => f()); }, reply(r: Record<string, unknown>) { next = r; } };
}

describe("question cards", () => {
  it("the review delay counts only while the card is active", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(approval());
    const approve = () => q.view()!.view.approve;
    expect([q.label(approve()), q.enabled(approve())]).toEqual(["Approve once (3)", false]);
    f.advance(5000);
    expect(q.label(approve())).toBe("Approve once (3)");
    q.setActive(true);
    f.advance(1000);
    expect(q.label(approve())).toBe("Approve once (2)");
    expect(q.enabled(q.view()!.view.deny)).toBe(true);
    f.advance(2000);
    expect([q.label(approve()), q.enabled(approve())]).toEqual(["Approve once", true]);
    q.setActive(false);
    expect(q.label(approve())).toBe("Approve once (3)");
    dispose();
  }));

  it("a rebuilt card starts the review again, and only a review card ticks", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(approval());
    q.setActive(true);
    f.advance(3000);
    expect(q.label(q.view()!.view.approve)).toBe("Approve once");
    q.apply(approval(2));
    f.advance(100);
    expect(q.label(q.view()!.view.approve)).toBe("Approve once (3)");
    q.apply(card());
    expect(f.ticking()).toBe(0);
    dispose();
  }));

  it("an unavailable card holds the review at its start", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    const off = approval();
    off.view.available = false;
    q.apply(off);
    q.setActive(true);
    f.advance(3000);
    // Qt restarts the clock while the card is not available (QuestionCard.cpp:239-240).
    expect(q.label(q.view()!.view.approve)).toBe("Approve once (3)");
    q.apply(approval());
    f.advance(1000);
    expect(q.label(q.view()!.view.approve)).toBe("Approve once (2)");
    dispose();
  }));

  it("a grown epoch alone keeps the review running", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(approval());
    q.setActive(true);
    f.advance(2000);
    // A draft load hands Rust's answers over without rebuilding the card (QuestionCard.cpp:289-301).
    q.apply({ ...approval(), epoch: 5 });
    f.advance(1000);
    expect(q.label(q.view()!.view.approve)).toBe("Approve once");
    dispose();
  }));

  it("every edit goes at once, with the epoch the fields show", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(card({}, 1, 4));
    q.edit({ kind: "text", item: "q_0", text: "L" });
    q.edit({ kind: "text", item: "q_0", text: "Li" });
    q.apply(card({}, 1, 6));
    q.edit({ kind: "option", item: "q_0", option: "opt_0_0", checked: true });
    expect(f.calls).toEqual([
      'edit 4 {"kind":"text","item":"q_0","text":"L"}',
      'edit 4 {"kind":"text","item":"q_0","text":"Li"}',
      'edit 6 {"kind":"option","item":"q_0","option":"opt_0_0","checked":true}',
    ]);
    dispose();
  }));

  it("buttons that are off send nothing", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(card({ submit: button({ enabled: false }) }));
    await q.submit();
    q.apply(approval());
    await q.decide(true);
    expect(f.calls).toEqual([]);
    await q.decide(false);
    expect(f.calls).toEqual(["decide false"]);
    dispose();
  }));

  it("holding Enter never submits again", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(card());
    await q.enter(true);
    expect(f.calls).toEqual([]);
    await q.enter(false);
    expect(f.calls).toEqual(["submit"]);
    dispose();
  }));

  it("a cold reply opens the dialog, and Cancel answers it", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(card());
    f.reply({ kind: "cold", token: 7, prompt: { title: "Submit with a cold cache?", text: "…", proceed: "Submit with full context" } });
    await q.submit();
    expect(q.dialog()?.prompt.title).toBe("Submit with a cold cache?");
    await q.answerCold(false);
    expect(q.dialog()).toBeUndefined();
    expect(f.calls).toEqual(["submit", "cold 7 false"]);
    dispose();
  }));

  it("a cold question goes with its card: Cancel, nothing sent", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(card());
    f.reply({ kind: "cold", token: 9, prompt: { title: "Submit with a cold cache?", text: "…", proceed: "Submit with full context" } });
    await q.submit();
    q.apply(card({}, 2));
    expect(q.dialog()?.token).toBe(9);
    q.apply({ ...card(), card: "question\nanother" });
    expect(q.dialog()).toBeUndefined();
    expect(f.calls).toEqual(["submit", "cold 9 false"]);
    dispose();
  }));

  it("a cold reply arriving after its card changed is cancelled without showing", () => createRoot(async (dispose) => {
    const f = fakes();
    let answer!: (reply: Awaited<ReturnType<QuestionPort["submit"]>>) => void;
    const q = createQuestions({ ...f.port, submit: () => new Promise((resolve) => { answer = resolve; }) }, f.clock);
    q.apply(card());
    const pending = q.submit();
    q.apply({ ...card(), card: "question\nanother" });
    answer({ kind: "cold", token: 10, prompt: { title: "Submit with a cold cache?", text: "…", proceed: "Submit with full context" } });
    await pending;
    expect(q.dialog()).toBeUndefined();
    expect(f.calls).toEqual(["cold 10 false"]);
    dispose();
  }));

  it("a cold reply arriving after unmount is cancelled even if the same card returns", () => createRoot(async (dispose) => {
    const f = fakes();
    let answer!: (reply: Awaited<ReturnType<QuestionPort["submit"]>>) => void;
    const q = createQuestions({ ...f.port, submit: () => new Promise((resolve) => { answer = resolve; }) }, f.clock);
    q.apply(card());
    const pending = q.submit();
    q.cancelCold();
    q.apply(card());
    answer({ kind: "cold", token: 11, prompt: { title: "Submit with a cold cache?", text: "…", proceed: "Submit with full context" } });
    await pending;
    expect(q.dialog()).toBeUndefined();
    expect(f.calls).toEqual(["cold 11 false"]);
    dispose();
  }));

  it("a notice reply raises a sticky error", () => createRoot(async (dispose) => {
    const f = fakes();
    const q = createQuestions(f.port, f.clock);
    q.apply(card());
    f.reply({ kind: "notice", text: "The question or your answers changed. Your draft was kept; review it before submitting again." });
    await q.submit();
    expect(f.calls.at(-1)).toBe("notify The question or your answers changed. Your draft was kept; review it before submitting again.");
    dispose();
  }));
});
