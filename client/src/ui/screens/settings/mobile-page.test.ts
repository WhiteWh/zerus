import { describe, expect, it } from "vitest";
import { createMobile, type MobileState, type View } from "./mobile-page";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const settle = () => new Promise((done) => setTimeout(done, 0));
const view = (change: Partial<View> = {}): View => ({
  workspace: null, workspace_error: null, workspace_busy: false, phones: null, phones_busy: false,
  delivery: "away_5", delivery_busy: false, delivery_status: "", delivery_outdated: null, delivery_worker: null, delivery_failed: false,
  version: 1, ...change,
});

function harness() {
  const calls: { name: string; arg?: string; reply: ReturnType<typeof deferred<View>> }[] = [];
  const call = (name: string, arg?: string) => { const reply = deferred<View>(); calls.push({ name, arg, reply }); return reply.promise; };
  let shown: MobileState = {};
  const page = createMobile({
    view: () => call("view"),
    open: () => call("open"),
    refreshWorkspace: () => call("workspace"),
    refreshPhones: () => call("phones"),
    setDelivery: (value) => call("delivery", value),
  }, (state) => { shown = state; });
  return { page, calls, shown: () => shown };
}

describe("the Mobile connection page", () => {
  it("never lets an older reply replace a newer view", async () => {
    const h = harness();
    h.page.open();
    h.page.changed();
    h.calls[1].reply.resolve(view({ version: 5, phones_busy: false }));
    await settle();
    h.calls[0].reply.resolve(view({ version: 3, phones_busy: true }));
    await settle();
    expect(h.shown().view?.version).toBe(5);
    expect(h.shown().view?.phones_busy).toBe(false);
  });

  it("asks for nothing the app is already doing", async () => {
    const h = harness();
    h.page.open();
    h.calls[0].reply.resolve(view({ workspace_busy: true, phones_busy: true, delivery_busy: true }));
    await settle();
    h.page.refreshWorkspace();
    h.page.refreshPhones();
    h.page.setDelivery("away");
    expect(h.calls).toHaveLength(1);
  });

  it("a known effect receipt reads recovery status even before the busy=false event", async () => {
    const h = harness(); h.page.open();
    h.calls[0].reply.resolve(view({version:3,workspace_busy:true})); await settle();
    h.page.refreshWorkspace(true);
    expect(h.calls.map(c=>c.name)).toEqual(["open","workspace"]);
    h.calls[1].reply.resolve(view({version:5,workspace_busy:false})); await settle();
    expect(h.shown().view?.workspace_busy).toBe(false);
  });

  it("offers no choice while the timing could not be read", async () => {
    const h = harness();
    h.page.open();
    h.calls[0].reply.resolve(view({ delivery_failed: true, delivery_status: "Could not read the shared setting: timed out" }));
    await settle();
    h.page.setDelivery("away");
    expect(h.calls).toHaveLength(1);
  });

  it("sends a new timing only, and shows a call that failed outright", async () => {
    const h = harness();
    h.page.setDelivery("away");
    expect(h.calls).toHaveLength(0);
    h.page.open();
    h.calls[0].reply.resolve(view());
    await settle();
    h.page.setDelivery("away_5");
    expect(h.calls).toHaveLength(1);
    h.page.setDelivery("away_10");
    expect(h.calls[1]).toMatchObject({ name: "delivery", arg: "away_10" });
    h.calls[1].reply.reject("command mobile_delivery_set not found");
    await settle();
    expect(h.shown().error).toBe("command mobile_delivery_set not found");
    h.page.changed();
    h.calls[2].reply.resolve(view({ version: 2 }));
    await settle();
    expect(h.shown().error).toBeUndefined();
  });
});
