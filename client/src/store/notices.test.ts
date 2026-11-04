import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createRoot } from "solid-js";
import { MAX_NOTICES, NOTICE_MS, bus, createNoticeBus, toast } from "./notices";

describe("the window's notice bus", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it("shows a notice for the design's toast time; a sticky one stays until dismissed", () => {
    createRoot((dispose) => {
      const b = createNoticeBus();
      b.notify("info", "Session ID copied");
      const error = b.notify("error", "Stop failed. Open Terminal to check the agent.", true);
      vi.advanceTimersByTime(NOTICE_MS - 1);
      expect(b.notices().map((n) => n.text)).toEqual(["Session ID copied", "Stop failed. Open Terminal to check the agent."]);
      vi.advanceTimersByTime(1);
      expect(b.notices().map((n) => n.tone)).toEqual(["error"]);
      vi.advanceTimersByTime(60_000);
      expect(b.notices()).toHaveLength(1);
      b.dismiss(error);
      expect(b.notices()).toEqual([]);
      dispose();
    });
  });

  it("shows a repeated passing message once, for the time of its latest copy", () => {
    createRoot((dispose) => {
      const b = createNoticeBus();
      b.notify("info", "Session updated. Refreshing…");
      vi.advanceTimersByTime(NOTICE_MS - 100);
      b.notify("info", "Session updated. Refreshing…");
      b.notify("info", "Session updated. Refreshing…");
      b.notify("warn", "Session updated. Refreshing…");
      b.notify("info", "Folder removed", false, undefined, "clean");
      expect(b.notices().map((n) => [n.tone, n.text])).toEqual([
        ["info", "Session updated. Refreshing…"], ["warn", "Session updated. Refreshing…"], ["info", "Folder removed"],
      ]);
      vi.advanceTimersByTime(100);
      expect(b.notices()).toHaveLength(3);
      vi.advanceTimersByTime(NOTICE_MS);
      expect(b.notices()).toEqual([]);
      // Errors that wait for the reader still show once per refusal.
      b.notify("error", "Stop failed.", true);
      b.notify("error", "Stop failed.", true);
      b.notify("error", "Stop failed.");
      expect(b.notices().map((n) => n.sticky)).toEqual([true, true, false]);
      dispose();
    });
  });

  it("replaces a notice with the same key and caps the stack, oldest passing notice first", () => {
    createRoot((dispose) => {
      const b = createNoticeBus();
      b.notify("info", "Marking 1 reply read…", false, "marks");
      b.notify("info", "1 reply marked as read.", false, "marks");
      expect(b.notices().map((n) => n.text)).toEqual(["1 reply marked as read."]);
      b.clearNotice("marks");
      expect(b.notices()).toEqual([]);
      b.notify("error", "kept", true);
      for (let i = 0; i < MAX_NOTICES; i++) b.notify("info", `passing ${i}`);
      expect(b.notices()).toHaveLength(MAX_NOTICES);
      expect(b.notices()[0].text).toBe("kept");
      expect(b.notices().at(-1)!.text).toBe(`passing ${MAX_NOTICES - 1}`);
      dispose();
    });
  });

  it("a toast is a titled notice on the window's bus", () => {
    createRoot((dispose) => {
      for (const n of bus.notices()) bus.dismiss(n.id);
      toast("Copied", "The reply is on the clipboard");
      toast("Updates", "The catalog could not be read", "error", true);
      expect(bus.notices()).toMatchObject([
        { tone: "info", title: "Copied", text: "The reply is on the clipboard", sticky: false },
        { tone: "error", title: "Updates", text: "The catalog could not be read", sticky: true },
      ]);
      vi.advanceTimersByTime(NOTICE_MS);
      expect(bus.notices().map((n) => n.title)).toEqual(["Updates"]);
      for (const n of bus.notices()) bus.dismiss(n.id);
      dispose();
    });
  });
});


it("a background notice or key clear cannot remove a pending Undo", () => {
  const bus = createNoticeBus();
  const id = bus.notify("info", "session", true, "session-end", "Archiving session", { dismissible: false });
  bus.notify("warn", "Earlier cleanup result", true, "session-end");
  bus.clearNotice("session-end");
  expect(bus.notices()).toMatchObject([{ id, dismissible: false }]);
  bus.dismiss(id); expect(bus.notices()).toEqual([]);
});
