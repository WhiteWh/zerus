import { describe, expect, it } from "vitest";
import { createNativeUi, NATIVE_FAILED, NATIVE_OFFLINE, type NativeBinding, type NativeCalls, type NativeRequest, type NativeStatus } from "./native-ui-model";

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const binding = (run = "run-1"): NativeBinding => ({ catalog_key: `run:${run}`, target: { computer: "build-01", name: "dsh/sample/migrate", run_id: run, conversation_id: "conversation-1" } });
const reply = (request: NativeRequest, status: NativeStatus["status"] = "ready", revision = 1): NativeStatus => ({ request, revision, status, text: status });
const rect = { x: 100, y: 150, width: 600, height: 500 };
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (reason: unknown) => void; const promise = new Promise<T>((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
function fixture() {
  const opens: { request: NativeRequest; reload: boolean; result: ReturnType<typeof deferred<NativeStatus>> }[] = [];
  const visible: { nonce: string; value: boolean }[] = [], browsers: NativeRequest[] = [];
  let retire = async () => {};
  let show = async (_value: boolean) => {};
  let browse = async () => {};
  let retired = 0, serial = 0;
  const calls: NativeCalls = {
    open: (request, reload) => { const result = deferred<NativeStatus>(); opens.push({ request, reload, result }); return result.promise; },
    retire: async () => { retired++; await retire(); }, bounds: async () => {},
    visible: async (nonce, value) => { visible.push({ nonce, value }); await show(value); },
    browser: async (request) => { browsers.push(request); await browse(); },
  };
  const model = createNativeUi(calls, () => `nonce-${++serial}`);
  const start = async () => { model.select(binding(), true); model.opened(true); await tick(); model.rectangle(rect); };
  return { model, opens, visible, browsers, start, retired: () => retired,
    onRetire: (fn: typeof retire) => { retire = fn; }, onShow: (fn: typeof show) => { show = fn; },
    onBrowser: (fn: typeof browse) => { browse = fn; } };
}

describe("native workspace identity and bounded exchange", () => {
  it("looks up only on opening, coalesces churn, and rejects the previous exact identity", async () => {
    const f = fixture();
    f.model.select(binding(), true); await tick(); expect(f.opens).toHaveLength(0);
    f.model.opened(true); await tick();
    const first = f.opens[0];
    for (let i = 2; i <= 200; i++) f.model.select(binding(`run-${i}`), true);
    await tick(); expect(f.opens).toHaveLength(1); expect(f.retired()).toBe(1);
    first.result.resolve(reply(first.request)); await tick();
    expect(f.opens).toHaveLength(2);
    expect(f.opens[1].request.target.run_id).toBe("run-200");
    expect(f.model.status().status).toBe("connecting");
    f.model.apply(reply(first.request, "failed", 999));
    expect(f.model.status().status).toBe("connecting");
    f.opens[1].result.resolve(reply(f.opens[1].request)); await tick();
    expect(f.model.status().status).toBe("ready");
  });

  it("sends foreground intent only for measured bounds; offline invalidates late callbacks", async () => {
    const f = fixture(); await f.start();
    await tick();
    expect(f.model.status().status).toBe("connecting");
    expect(f.visible.at(-1)?.value).toBe(true); // Rust still hides a non-Ready renderer.
    f.opens[0].result.resolve(reply(f.opens[0].request)); await tick();
    expect(f.visible.at(-1)?.value).toBe(true);
    f.model.select(binding(), false); await tick();
    expect(f.retired()).toBe(1);
    expect(f.model.status().text).toBe(NATIVE_OFFLINE);
    f.model.apply(reply(f.opens[0].request, "ready", 100));
    expect(f.model.status().status).toBe("offline");
    expect(f.opens).toHaveLength(1);
  });

  it("waits for a pending show and then its hide before acknowledging an overlay", async () => {
    const f = fixture(); await f.start();
    const show = deferred<void>();
    f.onShow(async (value) => { if (value) await show.promise; });
    f.opens[0].result.resolve(reply(f.opens[0].request)); await tick();
    expect(f.visible.at(-1)?.value).toBe(true);
    let hidden = false;
    const barrier = f.model.suspend(true).then(() => { hidden = true; });
    await tick(); expect(hidden).toBe(false);
    show.resolve(); await barrier;
    expect(f.visible.at(-1)?.value).toBe(false);
    f.model.rectangle({ ...rect, width: 610 }); await tick();
    expect(f.visible.at(-1)?.value).toBe(false);
    await f.model.suspend(false);
    expect(f.visible.at(-1)?.value).toBe(true);
  });

  it("never grants a replacement after retirement failed", async () => {
    const f = fixture(); await f.start();
    f.opens[0].result.resolve(reply(f.opens[0].request)); await tick();
    f.onRetire(async () => { throw new Error("private fixture failure"); });
    f.model.select(binding("run-2"), true); await tick();
    expect(f.opens).toHaveLength(1);
    expect(f.model.status().text).toBe(NATIVE_FAILED);
    await expect(f.model.suspend(true)).rejects.toThrow("private fixture failure");
  });

  it("requires an explicit menu intent for browser fallback, even when events precede the lookup reply", async () => {
    const f = fixture(); await f.start();
    const first = f.opens[0];
    f.model.apply(reply(first.request, "browser", 2)); await tick();
    expect(f.browsers).toHaveLength(0);
    f.model.fromMenu(); await tick(); expect(f.browsers).toHaveLength(0);
    first.result.resolve(reply(first.request, "browser", 2)); await tick();
    expect(f.browsers).toEqual([first.request]);
    f.model.apply(reply(first.request, "browser", 3)); await tick();
    expect(f.browsers).toHaveLength(1);
    f.model.reload(); await tick();
    expect(f.opens[1].reload).toBe(true);
    expect(f.opens[1].request.nonce).not.toBe(first.request.nonce);
    f.opens[1].result.resolve(reply(f.opens[1].request, "browser")); await tick();
    expect(f.browsers).toHaveLength(1);
  });

  it("rejects duplicate revisions, changed conversations and old reload events", async () => {
    const f = fixture(); await f.start();
    const first = f.opens[0];
    first.result.resolve(reply(first.request, "ready", 4)); await tick();
    f.model.apply(reply(first.request, "failed", 4));
    f.model.apply(reply({ ...first.request, target: { ...first.request.target, conversation_id: "other" } }, "failed", 5));
    expect(f.model.status().status).toBe("ready");
    f.model.reload(); await tick();
    f.model.apply(reply(first.request, "ready", 99));
    expect(f.model.status().status).toBe("connecting");
    f.opens[1].result.resolve(reply(f.opens[1].request, "stopped")); await tick();
    const nonce = f.model.request()!.nonce;
    f.model.opened(false); f.model.opened(true); await tick();
    expect(f.model.request()?.nonce).not.toBe(nonce);
    expect(f.opens).toHaveLength(3);
  });

  it("coalesces reload behind the original pending browser call without repeating it", async () => {
    const f = fixture(); await f.start();
    f.opens[0].result.resolve(reply(f.opens[0].request, "browser")); await tick();
    const browser = deferred<void>(); f.onBrowser(() => browser.promise);
    f.model.fromMenu(); await tick(); expect(f.browsers).toHaveLength(1);
    f.model.fromMenu(); f.model.reload(); await tick();
    expect(f.browsers).toHaveLength(1); expect(f.opens).toHaveLength(1);
    browser.resolve(); await tick();
    expect(f.opens).toHaveLength(2); expect(f.opens[1].reload).toBe(true);
  });

  it("ignores invalid bounds and hides on leaving the tab without another lookup", async () => {
    const f = fixture(); await f.start();
    f.model.rectangle({ ...rect, width: Number.NaN });
    f.opens[0].result.resolve(reply(f.opens[0].request)); await tick();
    expect(f.visible.some((call) => call.value)).toBe(false);
    f.model.rectangle(rect); await tick(); expect(f.visible.at(-1)?.value).toBe(true);
    f.model.opened(false); await tick(); expect(f.visible.at(-1)?.value).toBe(false);
    expect(f.opens).toHaveLength(1);
  });

  it("retains foreground intent during download navigation, but clears it for overlays and leaving", async () => {
    const f = fixture(); await f.start();
    f.model.rectangle(rect);
    f.opens[0].result.resolve(reply(f.opens[0].request)); await tick();
    f.model.apply(reply(f.opens[0].request, "loading", 2)); await tick();
    expect(f.model.status().status).toBe("loading");
    expect(f.visible.at(-1)?.value).toBe(true);
    await f.model.suspend(true); expect(f.visible.at(-1)?.value).toBe(false);
    await f.model.suspend(false); expect(f.visible.at(-1)?.value).toBe(true);
    f.model.opened(false); await tick(); expect(f.visible.at(-1)?.value).toBe(false);
  });
});
