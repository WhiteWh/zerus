import { describe, expect, it, vi } from "vitest";
import { createEffect, createRoot, untrack } from "solid-js";
import { createSessionModels, type ModelsPort, type PendingRow } from "./sessionModels";
import { effortChoices, modelChoices, settingsView, type ModelContext } from "../ui/sessions/session-model-settings";
import { toSession } from "./catalog";
import type { Outcome } from "../types";

function context(extra: Record<string, unknown> = {}, cmd = "codex"): ModelContext {
  const row = toSession("", "run:r1", { name: `${cmd}/sample-project/a`, run_id: "r1", conversation_id: "c1", agent: cmd,
    model: "model-a", effort: "high", state: "running", activity: "idle", phase: "idle", process_state: "running" });
  return { row, online: true, busy: false, details: { run_id: "r1", conversation_id: "c1", model: "model-a", effort: "high",
    settings_change_supported: true, settings_apply_when: "now", model_options: [
      { id: "model-a", label: "Model A", effort_options: ["low", "high"] },
      { id: "model-b", label: "Model B", effort_options: ["medium"] },
    ], ...extra } };
}
const tick = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
function setup() {
  const port: ModelsPort = { apply: vi.fn(), refresh: vi.fn(), catalog: vi.fn(), favorite: vi.fn(), notice: vi.fn() };
  const store = createSessionModels(port);
  const rows = (input: ModelContext): PendingRow[] => [{ key: input.row.key, row: input.row, online: input.online, busy: input.busy }];
  return { store, port: vi.mocked(port), rows };
}
const done = (status = "applied", id = "pending-1"): Outcome => ({ outcome: "done", cleanup: null,
  receipt: { status, request_id: "request-1", pending_settings_id: id, model: "model-b", effort: "medium" } });

describe("session model settings presentation", () => {
  it("keeps the current model/effort label and shows the pending change and Qt hint", () => {
    const v = settingsView(context({ pending_model: "model-b", pending_effort: "medium", settings_apply_when: "ready" }));
    expect(v.shownModel).toBe("model-a");
    expect(v.suffix).toBe("  high  ◷");
    expect(v.tooltip).toBe("Current: model-a  high\nPending: model-b  medium");
    expect(v.applyLabel).toBe("Save change");
    expect(v.hint).toContain("Messages sent while it is working use the current model.");
    expect(settingsView(context({ settings_apply_when: "resume" })).applyLabel).toBe("Save for resume");
  });
  it("separates the mockup's warning note from the settings hint", () => {
    const native = settingsView(context({ settings_change_notice: "Native re-read note" }, "claude"));
    expect(native.changeNotice).toBe("Native re-read note");
    expect(native.hint).not.toContain("Native re-read note");
    expect(settingsView(context({}, "claude")).changeNotice).toBe("Claude re-reads the conversation with the new model on its next turn.");
    expect(settingsView(context()).changeNotice).toBe("");
  });
  it("uses Qt's offline, busy and unsupported texts, with Terminal for Claude", () => {
    expect(settingsView({ ...context(), online: false }).reason).toBe("Machine unavailable. Reconnect to change model settings.");
    expect(settingsView(context(), true).reason).toBe("Updating model settings…");
    expect(settingsView(context({ settings_change_supported: false, settings_change_reason: "Use the native chooser." }), false).hint).toBe("Use the native chooser.");
    expect(settingsView(context({ settings_change_supported: false }, "claude")).terminal).toBe(true);
    expect(settingsView(context({ settings_change_supported: false }, "opencode")).terminal).toBe(false);
  });
  it("keeps native catalog order and switches effort lists with a model", () => {
    const choices = modelChoices(context().details?.model_options);
    expect(choices.map((m) => m.id)).toEqual(["model-a", "model-b"]);
    expect(effortChoices(choices, "model-b", "high", false, "model-a", "high")).toEqual({ choices: [{ value: "medium", label: "medium" }], value: "medium" });
    expect(effortChoices([], "unknown", "", false, "unknown", "").choices).toEqual([{ value: "", label: "Provider default" }]);
  });
  it("OpenCode offers Default, validated labeled variants and the current unknown variant", () => {
    const options = modelChoices([{ id: "provider/a", effort_options: ["high", "bad/value"], effort_labels: ["High", "Bad"] }]);
    expect(effortChoices(options, "provider/a", "custom", true, "provider/a", "custom")).toEqual({
      choices: [{ value: "", label: "Default" }, { value: "high", label: "High" }, { value: "custom", label: "custom" }], value: "custom",
    });
    expect(modelChoices(Array.from({ length: 300 }, (_, i) => ({ id: `model-${i}` })))).toHaveLength(256);
  });
});

describe("session model settings", () => {
  it("keeps an unknown native effort unselected until an explicit choice, without a default-low write", async () => {
    const { store, port } = setup();
    const fresh = context({ effort: "" }, "claude");
    store.setContext(fresh); store.open();
    expect(store.effort()).toBe("");
    expect(store.canApply()).toBe(false);
    await store.apply(); expect(port.apply).not.toHaveBeenCalled();
    store.chooseModel("model-b");
    expect(store.effort()).toBe("");
    expect(store.canApply()).toBe(false);
    store.chooseEffort("medium"); expect(store.canApply()).toBe(true);
    port.apply.mockResolvedValue(done());
    await store.apply();
    expect(port.apply).toHaveBeenCalledExactlyOnceWith({ computer: "", name: "claude/sample-project/a", run_id: "r1", conversation_id: "c1" }, "model-b", "medium", undefined, false);
    store.setContext(fresh);
    expect([store.view()?.model, store.view()?.effort]).toEqual(["model-b", "medium"]);
  });
  it("one Apply on a Ready session sends the manual change while its queued key is observed", async () => {
    const { store, port } = setup();
    const fresh = context({ effort: "" }, "claude");
    // session.ts wires view() and queuedKeys() to observe() in its own root. An
    // Apply click runs outside any update, so publishing the new key runs this
    // effect synchronously, before apply() sends.
    const dispose = createRoot((dispose) => {
      createEffect(() => {
        store.view();
        const rows = store.queuedKeys().map((key) => ({ key, row: key === fresh.row.key ? fresh.row : undefined, online: true, busy: false }));
        untrack(() => store.observe(rows));
      });
      return dispose;
    });
    // hgs takes the pending path for any expected_pending_id and, with no
    // pending settings, refuses it before writing a receipt.
    const refused = vi.fn();
    port.apply.mockImplementation(async (_target, model, effort, pending) => {
      if (pending !== undefined) {
        refused(pending);
        return { outcome: "failed", detail: "hgs: pending settings target changed; refresh before applying" };
      }
      return { outcome: "done", cleanup: null, receipt: { status: "applied", request_id: "request-1", model, effort } };
    });
    store.setContext(fresh); store.open(); store.chooseModel("model-b"); store.chooseEffort("medium");
    expect(store.canApply()).toBe(true);
    await store.apply(); await tick();
    expect(refused).not.toHaveBeenCalled();
    expect(port.apply).toHaveBeenCalledExactlyOnceWith({ computer: "", name: "claude/sample-project/a", run_id: "r1", conversation_id: "c1" }, "model-b", "medium", undefined, false);
    expect(port.notice).not.toHaveBeenCalled();
    expect(store.opened()).toBe(false);
    expect([store.view()?.model, store.view()?.effort]).toEqual(["model-b", "medium"]);
    dispose();
  });
  it("a manual Apply goes first when another session's scheduled change turns Ready as its key is published", async () => {
    const { store, port } = setup();
    const a = context({ effort: "" }, "claude");
    const bRow = toSession("", "run:r2", { name: "codex/sample-project/b", run_id: "r2", conversation_id: "c2", agent: "codex",
      model: "model-a", effort: "high", state: "running", activity: "busy", phase: "working", process_state: "running" });
    const b: ModelContext = { row: bRow, online: true, busy: false, details: { run_id: "r2", conversation_id: "c2", model: "model-a", effort: "high",
      settings_change_supported: true, settings_apply_when: "ready", pending_model: "model-b", pending_effort: "medium",
      pending_settings_id: "pending-b", model_options: a.details?.model_options } };
    // The catalog rows session.ts's effect reads when it runs; not reactive.
    const catalogRows = new Map([[a.row.key, a.row], [b.row.key, b.row]]);
    const dispose = createRoot((dispose) => {
      createEffect(() => {
        store.view();
        const rows = store.queuedKeys().map((key) => ({ key, row: catalogRows.get(key), online: true, busy: false }));
        untrack(() => store.observe(rows));
      });
      return dispose;
    });
    let finishA: ((outcome: Outcome) => void) | undefined;
    port.apply.mockImplementation((target, model, effort) => target.run_id === "r1"
      ? new Promise<Outcome>((resolve) => { finishA = resolve; })
      : Promise.resolve({ outcome: "done", cleanup: null, receipt: { status: "applied", request_id: "request-b", model, effort } }));
    // B's host-scheduled change was queued while B was busy; then A is selected.
    store.setContext(b); store.setContext(a);
    expect(store.queuedKeys()).toEqual([b.row.key]);
    store.open(); store.chooseModel("model-b"); store.chooseEffort("medium");
    expect(port.apply).not.toHaveBeenCalled();
    // B becomes Ready exactly when A's key is published.
    catalogRows.set(b.row.key, { ...b.row, activity: "idle", phase: "idle" });
    const applying = store.apply();
    expect(port.apply).toHaveBeenCalledExactlyOnceWith({ computer: "", name: "claude/sample-project/a", run_id: "r1", conversation_id: "c1" }, "model-b", "medium", undefined, false);
    await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
    finishA?.({ outcome: "done", cleanup: null, receipt: { status: "applied", request_id: "request-a", model: "model-b", effort: "medium" } });
    await applying; await tick();
    expect(port.apply).toHaveBeenCalledTimes(2);
    expect(port.apply).toHaveBeenLastCalledWith({ computer: "", name: "codex/sample-project/b", run_id: "r2", conversation_id: "c2" }, "model-b", "medium", "pending-b", false);
    expect(port.notice).not.toHaveBeenCalled();
    expect([store.view()?.model, store.view()?.effort]).toEqual(["model-b", "medium"]);
    dispose();
  });
  it("stages changes, cancels without a write, then applies once with exact identities", async () => {
    await createRoot(async (dispose) => {
      const { store, port } = setup(); store.setContext(context());
      store.open(); expect(store.canApply()).toBe(false);
      store.chooseModel("model-b"); expect(store.effort()).toBe("medium"); expect(store.canApply()).toBe(true);
      store.close(); expect(port.apply).not.toHaveBeenCalled();
      store.open(); expect(store.model()).toBe("model-a");
      store.chooseModel("model-b"); port.apply.mockResolvedValue(done());
      await store.apply(); await store.apply();
      expect(port.apply).toHaveBeenCalledExactlyOnceWith({ computer: "", name: "codex/sample-project/a", run_id: "r1", conversation_id: "c1" }, "model-b", "medium", undefined, false);
      expect(store.opened()).toBe(false); expect(store.view()?.model).toBe("model-b");
      dispose();
    });
  });
  it("a changed session closes the popup and an unsupported/offline session cannot apply", async () => {
    const { store, port } = setup(); store.setContext(context()); store.open(); store.chooseModel("model-b");
    store.setContext({ ...context(), row: { ...context().row, runId: "r2" }, details: { ...context().details, run_id: "r2" } });
    expect(store.opened()).toBe(false); await store.apply(); expect(port.apply).not.toHaveBeenCalled();
    store.setContext({ ...context(), online: false }); store.open(); store.chooseModel("model-b"); expect(store.canApply()).toBe(false);
  });
  it("shows a saved change until Ready and pins the automatic apply to its pending identity", async () => {
    const { store, port, rows } = setup();
    const busy = context({ settings_apply_when: "ready" }); busy.row.activity = "busy"; busy.row.phase = "working";
    store.setContext(busy); store.open(); store.chooseModel("model-b"); port.apply.mockResolvedValueOnce(done("scheduled"));
    await store.apply(); expect(store.view()?.pending).toBe(true);
    store.observe(rows(busy)); expect(port.apply).toHaveBeenCalledTimes(1);
    const ready = context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "pending-1" });
    store.setContext(ready); port.apply.mockResolvedValueOnce(done()); store.observe(rows(ready)); await tick();
    expect(port.apply).toHaveBeenLastCalledWith(expect.objectContaining({ run_id: "r1", conversation_id: "c1" }), "model-b", "medium", "pending-1", false);
    store.observe(rows(ready)); await tick(); expect(port.apply).toHaveBeenCalledTimes(2);
  });
  it("a refused or uncertain result never authorizes an automatic retry", async () => {
    for (const outcome of ["failed"] as const) {
      const { store, port, rows } = setup();
      const i = context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "pending-1" });
      store.setContext(i); port.apply.mockResolvedValue({ outcome, detail: "Check Terminal before retrying." });
      store.observe(rows(i)); await tick(); store.setContext(i); store.observe(rows(i)); await tick();
      expect(port.apply).toHaveBeenCalledTimes(1); expect(port.notice).toHaveBeenCalledTimes(1);
    }
  });
  it("an uncertain manual change cannot auto-apply a newly observed pending identity", async () => {
    const { store, port, rows } = setup();
    store.setContext(context()); store.open(); store.chooseModel("model-b");
    port.apply.mockRejectedValue("Settings change timed out. Check Terminal before retrying.");
    await store.apply();
    const later = context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "unconfirmed-request" });
    store.setContext(later); store.observe(rows(later)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
    expect(store.view()?.pending).toBe(true);
  });
  it("an unselected saved change still applies on its exact catalog Ready transition", async () => {
    const { store, port, rows } = setup();
    const i = context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "pending-1", settings_apply_when: "ready" });
    store.setContext(i); store.setContext(undefined); port.apply.mockResolvedValue(done());
    store.observe(rows(i)); await tick(); expect(port.apply).toHaveBeenCalledTimes(1);
  });

  it("a confirmed scheduled automatic apply needs a distinct NotReady to Ready transition", async () => {
    const { store, port, rows } = setup();
    const ready = context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "pending-1" });
    store.setContext(ready); port.apply.mockResolvedValue(done("scheduled"));
    store.observe(rows(ready)); await tick(); store.observe(rows(ready)); expect(port.apply).toHaveBeenCalledTimes(1);
    const busy = { ...ready, details: { ...ready.details, settings_apply_when: "ready" }, row: { ...ready.row, phase: "working", activity: "busy" } };
    store.setContext(busy); store.observe(rows(busy));
    store.setContext(ready); port.apply.mockResolvedValue(done()); store.observe(rows(ready)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(2);
  });
  it("does not apply a saved change to a different run or conversation", async () => {
    const { store, port, rows } = setup();
    const i = context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "pending-1", settings_apply_when: "ready" });
    store.setContext(i); store.observe(rows({ ...i, row: { ...i.row, runId: "other" } }));
    store.observe(rows({ ...i, row: { ...i.row, conversationId: "other" } })); expect(port.apply).not.toHaveBeenCalled();
  });
  it("loads the OpenCode catalog only on open and ignores a late answer from another context", async () => {
    const { store, port } = setup();
    let resolve!: (value: Record<string, unknown>) => void;
    port.catalog.mockReturnValue(new Promise((r) => { resolve = r; }));
    const i = context({ model_options_truncated: false, cwd: "/workspace/sample-project", model_options: [{ id: "provider/a" }] }, "opencode");
    store.setContext(i); expect(port.catalog).not.toHaveBeenCalled();
    store.open(); expect(port.catalog).toHaveBeenCalledExactlyOnceWith("", "/workspace/sample-project", "this computer");
    store.setContext(context()); resolve({ models: [{ value: "provider/late" }] }); await tick();
    expect(store.options().some((v) => v.id === "provider/late")).toBe(false);
  });
  it("OpenCode selection imports catalog variants before applying, including explicit Default", async () => {
    const { store, port } = setup();
    const i = context({ settings_model: "provider/a", model_options_truncated: false,
      model_options: [{ id: "provider/a", effort_options: ["high"] }] }, "opencode");
    port.catalog.mockResolvedValue({ models: [{ value: "provider/b", label: "Model B", effort_options: ["medium"] }] });
    store.setContext(i); store.open(); await tick();
    store.pickerActions.toggleAll(); store.pickerActions.select("provider/b");
    expect(store.model()).toBe("provider/b"); expect(store.efforts().map((e) => e.value)).toEqual(["", "medium"]);
    store.chooseEffort(""); port.apply.mockResolvedValue({ ...done(), receipt: { status: "applied", model: "provider/b", effort: "" } } as Outcome);
    await store.apply(); expect(port.apply).toHaveBeenLastCalledWith(expect.anything(), "provider/b", "", undefined, true);
  });
  it("keeps a confirmed favorite across stale details and a late catalog response", async () => {
    const { store, port } = setup();
    const i = context({ settings_model: "provider/a", model_options_truncated: false,
      model_options: [{ id: "provider/a", favorite: false }] }, "opencode");
    let resolve!: (value: Record<string, unknown>) => void;
    port.catalog.mockReturnValue(new Promise((r) => { resolve = r; }));
    port.favorite.mockResolvedValue({});
    store.setContext(i); store.open(); store.pickerActions.click("provider/a", true); await tick();
    expect(port.favorite).toHaveBeenCalledExactlyOnceWith("", "provider/a", true, "this computer");
    store.setContext(i);
    resolve({ models: [{ value: "provider/a", favorite: false }] }); await tick();
    store.pickerActions.toggleAll();
    expect(store.picker().groups.flatMap((g) => g.models).find((m) => m.value === "provider/a")?.favorite).toBe(true);
  });
  it("never retries a failed favorite and ignores its answer after changing context", async () => {
    const { store, port } = setup();
    const i = context({ model_options_truncated: false, model_options: [{ id: "provider/a", favorite: false }] }, "opencode");
    port.catalog.mockResolvedValue({ models: [] });
    port.favorite.mockRejectedValue(new Error("Check the model catalog before retrying."));
    store.setContext(i); store.open(); store.pickerActions.click("provider/a", true); await tick();
    store.setContext(i); expect(port.favorite).toHaveBeenCalledTimes(1);
    expect(store.catalogError()).toContain("Check the model catalog");
    let resolve!: (value: unknown) => void;
    port.favorite.mockReturnValue(new Promise((r) => { resolve = r; }));
    store.pickerActions.click("provider/a", true); store.setContext(context()); resolve({}); await tick();
    expect(store.picker().groups.flatMap((g) => g.models).some((m) => m.favorite)).toBe(false);
  });
});

describe("session model scheduled Settings keeper races", () => {
  const keeperBusy: Outcome = { outcome: "failed", detail: "hgs: Original picker host is busy or unavailable: Send holds it" };
  const pending = () => context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "pending-1" });

  it("settles silently when Send wins the keeper slot and consumes the pending change", async () => {
    const { store, port, rows } = setup();
    const i = pending();
    store.setContext(i);
    port.apply.mockResolvedValue(keeperBusy);
    port.refresh.mockResolvedValue({ run_id: "r1", conversation_id: "c1", pending_settings_id: "", model: "model-b", effort: "medium" });
    store.observe(rows(i)); await tick();
    expect(port.apply).toHaveBeenCalledExactlyOnceWith(expect.anything(), "model-b", "medium", "pending-1", false);
    expect(port.refresh).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ run_id: "r1", conversation_id: "c1" }));
    expect(port.notice).not.toHaveBeenCalled();
    expect(store.queuedKeys()).toEqual([]);
    store.observe(rows(i)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
  });

  it("does not refresh or replay when the automatic apply wins the race", async () => {
    const { store, port, rows } = setup();
    const i = pending();
    store.setContext(i); port.apply.mockResolvedValue(done());
    store.observe(rows(i)); await tick();
    store.setContext(context({ model: "model-b", effort: "medium" }));
    store.observe(rows(context({ model: "model-b", effort: "medium" }))); await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
    expect(port.refresh).not.toHaveBeenCalled();
    expect(port.notice).not.toHaveBeenCalled();
    expect(store.queuedKeys()).toEqual([]);
  });

  it("keeps a manual Apply keeper error visible and blocks automatic retry", async () => {
    const { store, port, rows } = setup();
    const i = context();
    store.setContext(i); store.open(); store.chooseModel("model-b");
    port.apply.mockResolvedValue(keeperBusy);
    await store.apply();
    expect(port.refresh).not.toHaveBeenCalled();
    expect(port.notice).toHaveBeenCalledTimes(1);
    store.observe(rows(i)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
  });

  it("rearms the same pending change only after a distinct NotReady to Ready transition", async () => {
    const { store, port, rows } = setup();
    const i = pending();
    store.setContext(i);
    port.apply.mockResolvedValueOnce(keeperBusy).mockResolvedValueOnce(done());
    port.refresh.mockResolvedValue({ run_id: "r1", conversation_id: "c1", pending_settings_id: "pending-1" });
    store.observe(rows(i)); await tick();
    expect(port.refresh).toHaveBeenCalledTimes(1);
    expect(port.notice).not.toHaveBeenCalled();
    store.observe(rows(i)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
    const notReady = { ...i, details: { ...i.details, settings_apply_when: "ready" }, row: { ...i.row, activity: "busy", phase: "working" } };
    store.observe(rows(notReady));
    store.observe(rows(i)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(2);
    expect(port.apply).toHaveBeenLastCalledWith(expect.anything(), "model-b", "medium", "pending-1", false);
    expect(port.refresh).toHaveBeenCalledTimes(1);
  });

  it("treats a replaced pending identity as stale without blocking its replacement", async () => {
    const { store, port, rows } = setup();
    const i = pending();
    store.setContext(i);
    port.apply.mockResolvedValueOnce(keeperBusy).mockResolvedValueOnce(done());
    port.refresh.mockResolvedValue({ run_id: "r1", conversation_id: "c1", pending_settings_id: "pending-2" });
    store.observe(rows(i)); await tick();
    expect(port.notice).not.toHaveBeenCalled();
    expect(store.queuedKeys()).toEqual([]);
    const replaced = context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "pending-2" });
    store.setContext(replaced); store.observe(rows(replaced)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(2);
    expect(port.apply).toHaveBeenLastCalledWith(expect.anything(), "model-b", "medium", "pending-2", false);
  });

  it("does not reconcile a keeper-busy result onto a changed target", async () => {
    const { store, port, rows } = setup();
    const i = pending();
    store.setContext(i); port.apply.mockResolvedValue(keeperBusy);
    port.refresh.mockResolvedValue({ run_id: "r2", conversation_id: "c2", pending_settings_id: "pending-1" });
    store.observe(rows(i)); await tick();
    expect(port.notice).not.toHaveBeenCalled();
    expect(store.queuedKeys()).toEqual([]);
    store.observe(rows(i)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
  });

  it("clears a waiting scheduled attempt when a later inspection consumes it", async () => {
    const { store, port, rows } = setup();
    const i = pending();
    store.setContext(i); port.apply.mockResolvedValue(keeperBusy);
    port.refresh.mockResolvedValue({ run_id: "r1", conversation_id: "c1", pending_settings_id: "pending-1" });
    store.observe(rows(i)); await tick();
    store.setContext(context({ model: "model-b", effort: "medium" }));
    expect(store.queuedKeys()).toEqual([]);
    store.observe(rows(context({ model: "model-b", effort: "medium" }))); await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
    expect(port.notice).not.toHaveBeenCalled();
  });
});

describe("session model uncertain Settings outcomes", () => {
  it("never refreshes an uncertain keeper result into silent settlement or replay", async () => {
    const { store, port, rows } = setup();
    const i = context({ pending_model: "model-b", pending_effort: "medium", pending_settings_id: "pending-1" });
    store.setContext(i);
    port.apply.mockResolvedValue({ outcome: "uncertain", detail: "hgs: Original picker host is busy or unavailable: Send holds it" });
    store.observe(rows(i)); await tick();
    expect(port.refresh).not.toHaveBeenCalled();
    expect(port.notice).toHaveBeenCalledTimes(1);
    expect(store.queuedKeys()).toEqual([i.row.key]);
    const notReady = { ...i, details: { ...i.details, settings_apply_when: "ready" }, row: { ...i.row, activity: "busy", phase: "working" } };
    store.observe(rows(notReady)); store.observe(rows(i)); await tick();
    expect(port.apply).toHaveBeenCalledTimes(1);
  });
});
