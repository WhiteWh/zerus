import { describe, expect, it, vi } from "vitest";
import catalog from "../i18n.ru.json";

// The Undo toast and its cancel notice in Russian, through the shipped catalog.
vi.mock("../i18n", async (original) => {
  const i18n = await original<typeof import("../i18n")>();
  return { ...i18n, tr: (source: string, ...args: (string | number)[]) => i18n.translateWithCatalog(catalog, "ru", source, ...args) };
});
const { createEndActions } = await import("./endActions");
const { createNoticeBus } = await import("./notices");
type EndCapture = import("./endActionBusy").EndCapture;

const capture = (name: string): EndCapture => ({ target: { computer: "test", name, run_id: `run-${name}`, conversation_id: `conversation-${name}` }, action: { kind: "archive" } as EndCapture["action"], state: "paused" });
const tick = async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); };

describe("end action toast in Russian", () => {
  it("translates the title, the overflow, the folder note, Undo and the cancel notice", async () => {
    const bus = createNoticeBus();
    let settle = (_: boolean) => {};
    const app = createEndActions({ begin: async () => {}, wait: () => new Promise((resolve) => { settle = resolve; }), cancel: async () => settle(false) }, bus, () => "id-1", () => Date.now());
    const job = app.schedule(["a/1", "a/2", "a/3", "a/4", "a/5"].map(capture), vi.fn(), { remove: true }); await tick();
    const notice = bus.notices()[0];
    expect(notice.title).toBe("Архивация сессий: 5");
    expect(notice.text).toBe("a/1, a/2, a/3 и ещё 2\nПосле этого рабочая папка будет удалена.");
    expect((notice.actions![0].label as () => string)()).toBe("Отменить (3)");
    await notice.actions![0].run(); await job;
    expect(bus.notices().at(-1)!.text).toBe("Архивация отменена");
  });
});
