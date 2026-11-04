import { describe, expect, it, vi } from "vitest";
import { emptyUpdatesView, type UpdatesView } from "../types";
import { createUpdates, type UpdatesPort } from "./updates";

function fake(overrides: Partial<UpdatesPort> = {}) {
  const port: UpdatesPort = {
    view: vi.fn(async () => emptyUpdatesView()),
    listen: vi.fn(async () => () => {}),
    check: vi.fn(async () => {}),
    setChannel: vi.fn(async () => {}),
    setAutomatic: vi.fn(async () => {}),
    open: vi.fn(async () => {}),
    copyCommand: vi.fn(async () => {}),
    restart: vi.fn(async () => {}),
    fixLoginItem: vi.fn(async () => null),
    ...overrides,
  };
  return port;
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("updates store", () => {
  it("listens before the first read and never lets that read erase an event", async () => {
    let answer!: (view: UpdatesView) => void;
    let changed!: (view: UpdatesView) => void;
    const available = { ...emptyUpdatesView(), available: true, heading: { source: "Update available" }, version: "0.47.0" };
    const port = fake({
      view: vi.fn(() => new Promise<UpdatesView>((resolve) => { answer = resolve; })),
      listen: vi.fn(async (listener) => { changed = listener; return () => {}; }),
    });
    const store = createUpdates(port);
    const started = store.start();
    await tick();
    expect(port.listen).toHaveBeenCalledBefore(port.view as ReturnType<typeof vi.fn>);
    changed(available);
    answer(emptyUpdatesView());
    await started;
    expect(store.view()).toEqual(available);
  });

  it("refresh only reads the snapshot and drops a read superseded by an event", async () => {
    let answer!: (view: UpdatesView) => void;
    let changed!: (view: UpdatesView) => void;
    let views = 0;
    const port = fake({
      view: vi.fn(() => ++views === 1 ? Promise.resolve(emptyUpdatesView()) : new Promise<UpdatesView>((resolve) => { answer = resolve; })),
      listen: vi.fn(async (listener) => { changed = listener; return () => {}; }),
    });
    const store = createUpdates(port);
    await store.start();
    const reading = store.refresh();
    changed({ ...emptyUpdatesView(), cached: true, error: { source: "Check failed. Try again later." } });
    answer(emptyUpdatesView());
    await reading;
    expect(store.view().cached).toBe(true);
    expect(port.check).not.toHaveBeenCalled();
    expect(port.view).toHaveBeenCalledTimes(2);
  });

  it("uses only the update commands for controls and actions", async () => {
    const port = fake();
    const store = createUpdates(port);
    await store.check();
    await store.setChannel("dev");
    await store.setAutomatic(false);
    await store.open("release");
    await store.open("download");
    await store.open("instructions");
    await store.copyCommand();
    await store.restart();
    await store.fixLoginItem(false);
    expect(port.check).toHaveBeenCalledTimes(1);
    expect(port.setChannel).toHaveBeenCalledWith("dev");
    expect(port.setAutomatic).toHaveBeenCalledWith(false);
    expect(port.open).toHaveBeenNthCalledWith(1, "release");
    expect(port.open).toHaveBeenNthCalledWith(2, "download");
    expect(port.open).toHaveBeenNthCalledWith(3, "instructions");
    expect(port.copyCommand).toHaveBeenCalledTimes(1);
    expect(port.restart).toHaveBeenCalledTimes(1);
    expect(port.fixLoginItem).toHaveBeenCalledWith(false);
  });
});
