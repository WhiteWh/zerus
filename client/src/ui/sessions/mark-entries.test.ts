import { beforeEach, describe, expect, it, vi } from "vitest";

const m = vi.hoisted(() => {
  // store/ui.ts reads the page address and storage when it loads.
  const store = new Map<string, string>();
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
  return {
  markRead: vi.fn(() => true), markReadUnavailable: vi.fn<() => string | undefined>(() => undefined), markNeedsAttention: vi.fn(() => true), markAllRead: vi.fn(() => 0), unreadReplies: vi.fn(() => new Map<string, string>()),
  notify: vi.fn(), stillListed: vi.fn((row: unknown) => row),
  };
});
vi.mock("../../store/marks", async (orig) => ({ ...(await orig<object>()), markRead: m.markRead, markReadUnavailable: m.markReadUnavailable, markNeedsAttention: m.markNeedsAttention, markAllRead: m.markAllRead, unreadReplies: m.unreadReplies }));
vi.mock("../../store/actions", async (orig) => ({ ...(await orig<object>()), notify: m.notify, stillListed: m.stillListed }));

import { markAllEntry, markAllLabel, markAllTip, markEntry, markReadChip } from "./mark-entries";
import { bindSearchRow, listedByFilter, readSelectionKey, setReadSelectionKey, setSelectedKey } from "../../store/ui";
import type { Session } from "./types";

const session = (extra: Record<string, unknown> = {}, reviewLater = false) => ({
  key: "run:a", computer: "", name: "claude/proj/a", state: "running", status: "ready", unread: false, reviewLater, ...extra,
}) as unknown as Session;
const unread = { unread: true };

beforeEach(() => { vi.clearAllMocks(); m.markRead.mockReturnValue(true); m.markReadUnavailable.mockReturnValue(undefined); m.markNeedsAttention.mockReturnValue(true); m.stillListed.mockImplementation((row: unknown) => row); });

describe("markReadChip", () => {
  it("is active for a live row that needs attention", () => {
    expect(markReadChip(session(unread), true).active).toBe(true);
    expect(markReadChip(session({}, true), true).active).toBe(true);
    expect(markReadChip(session(), true).active).toBe(false);
    expect(markReadChip(session({ ...unread, state: "archived" }), true).active).toBe(false);
  });
  it("is active offline only for a Review later mark (SessionPresentation.h:281)", () => {
    expect(markReadChip(session(unread), false).active).toBe(false);
    expect(markReadChip(session({}, true), false).active).toBe(true);
  });
  it("describes a review mark and a notification", () => {
    expect(markReadChip(session({}, true), true).detail).toBe("Marked for later. Mark as read to clear this reminder.");
    expect(markReadChip(session(unread), true).detail).toBe("Mark this session's current notification as read. This does not answer a pending question.");
  });
});

describe("markEntry", () => {
  it("offers Mark as read for a row that needs attention, always enabled", () => {
    const e = markEntry(session({}, true))!;
    expect(e).toMatchObject({ label: "Mark as read" });
    expect(e.disabled).toBeUndefined();
    e.action!();
    expect(m.markRead).toHaveBeenCalledTimes(1);
    expect(m.notify).not.toHaveBeenCalled();
  });
  it("offers Mark needs attention otherwise, with Qt's hint", () => {
    const e = markEntry(session())!;
    expect(e).toMatchObject({ label: "Mark needs attention", hint: "Keep a reminder to review this session later" });
    expect(e.disabled).toBeUndefined();
    e.action!();
    expect(m.markNeedsAttention).toHaveBeenCalledTimes(1);
    expect(m.notify).not.toHaveBeenCalled();
  });
  it("shows neither for an archived row", () => expect(markEntry(session({ state: "archived" }))).toBeUndefined());
  it("says why a refused mark did not run", () => {
    m.markRead.mockReturnValue(false);
    markEntry(session({}, true))!.action!();
    expect(m.notify).toHaveBeenLastCalledWith("error", "A new reply or question arrived. Open the menu again to mark it as read.", true);
    m.markNeedsAttention.mockReturnValue(false);
    markEntry(session())!.action!();
    expect(m.notify).toHaveBeenLastCalledWith("error", "This session is no longer available. Open its actions again.", true);
  });
});

describe("the click-time recheck", () => {
  it("runs first: a vanished or changed row calls no mark and the notice comes from the recheck", () => {
    m.stillListed.mockReturnValue(undefined);
    markEntry(session({}, true))!.action!();
    markEntry(session())!.action!();
    expect(m.stillListed).toHaveBeenCalledTimes(2);
    expect(m.markRead).not.toHaveBeenCalled();
    expect(m.markNeedsAttention).not.toHaveBeenCalled();
    expect(m.notify).not.toHaveBeenCalled();
  });
});

describe("markAllEntry", () => {
  it("counts the unread replies and is off at zero", () => {
    expect(markAllLabel(0)).toBe("Mark all as read");
    expect(markAllEntry()).toMatchObject({ label: "Mark all as read", disabled: true });
    m.unreadReplies.mockReturnValue(new Map([["a", "1"], ["b", "2"]]));
    const e = markAllEntry();
    expect(e).toMatchObject({ label: "Mark all as read (2)", disabled: false });
    expect(markAllTip(e)).toBe(`Mark all as read (2)\n${e.hint}`);
    expect(e.hint).toMatch(/^Mark current replies across all machines as read, including hidden sessions\./);
  });
  it("marks them and counts them in a notice", () => {
    m.unreadReplies.mockReturnValue(new Map([["a", "1"]]));
    m.markAllRead.mockReturnValue(1);
    markAllEntry().action!();
    expect(m.notify).toHaveBeenLastCalledWith("info", "1 reply marked as read.");
    m.markAllRead.mockReturnValue(3);
    markAllEntry().action!();
    expect(m.notify).toHaveBeenLastCalledWith("info", "3 replies marked as read.");
    m.notify.mockClear();
    m.markAllRead.mockReturnValue(0);
    markAllEntry().action!();
    expect(m.notify).not.toHaveBeenCalled();
  });
});

describe("the open row after a read (Qt's m_readSelectionKey)", () => {
  beforeEach(() => { setSelectedKey(undefined); setReadSelectionKey(undefined); });

  it("keeps the selected row after Mark as read, never another row or a refused mark", () => {
    setSelectedKey("run:a");
    markEntry(session({}, true))!.action!();
    expect(readSelectionKey()).toBe("run:a");
    setReadSelectionKey(undefined);
    markEntry(session({ key: "run:b" }, true))!.action!();
    expect(readSelectionKey()).toBeUndefined();
    m.markRead.mockReturnValue(false);
    markEntry(session({}, true))!.action!();
    expect(readSelectionKey()).toBeUndefined();
  });

  it("keeps the selected row with an unread reply after Mark all as read", () => {
    const rows: Record<string, Session> = { "run:u": session({ key: "run:u", unreadReply: true }), "run:r": session({ key: "run:r", unreadReply: false }) };
    bindSearchRow((key) => rows[key]);
    m.unreadReplies.mockReturnValue(new Map([["a", "1"]]));
    setSelectedKey("run:r");
    markAllEntry().action!();
    expect(readSelectionKey()).toBeUndefined();
    setSelectedKey("run:u");
    markAllEntry().action!();
    expect(readSelectionKey()).toBe("run:u");
    bindSearchRow(() => undefined);
  });

  it("lists the kept row under Needs attention and Active only while it is selected and kept", () => {
    // No computer is online here: this row matches neither filter by itself.
    const read = session({ key: "run:k" });
    expect(listedByFilter(read, "attention")).toBe(false);
    setSelectedKey("run:k");
    expect(listedByFilter(read, "attention")).toBe(false);
    setReadSelectionKey("run:k");
    expect(listedByFilter(read, "attention")).toBe(true);
    expect(listedByFilter(read, "active")).toBe(true);
    expect(listedByFilter(read, "paused")).toBe(false);
    expect(listedByFilter(read, "archived")).toBe(false);
    expect(listedByFilter(session({ key: "run:k", state: "archived" }), "attention")).toBe(false);
    expect(listedByFilter(session({ key: "run:other" }), "attention")).toBe(false);
    setSelectedKey("run:other");
    expect(listedByFilter(read, "attention")).toBe(false);
    expect(listedByFilter(read, "all")).toBe(true);
  });
});


describe("service mark-read refusal", () => {
  it("shows the explicit service/host reason instead of a stale-reply notice", () => {
    for (const reason of ["Read marks are unavailable until the background service is ready.", "This host has no shared read state for this session."]) {
      m.markReadUnavailable.mockReturnValue(reason);
      markEntry(session({}, true))!.action!();
      expect(m.notify).toHaveBeenLastCalledWith("error", reason, true);
      expect(m.markRead).not.toHaveBeenCalled();
    }
  });
});
