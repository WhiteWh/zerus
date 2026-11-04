import { afterEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // store/ui.ts reads the page address and storage when it loads.
  const store = new Map<string, string>();
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  vi.stubGlobal("navigator", { platform: "MacIntel" });
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
  // The list module registers Solid's delegated events when it loads; nothing renders here.
  // The composer it loads with Saved drafts listens for the page going.
  vi.stubGlobal("window", { document: { addEventListener: () => {} }, addEventListener: () => {} });
});
vi.mock("../../bridge", () => ({
  // The native workspace store, loaded by the session entry points.
  nativeUiOpen: vi.fn(), nativeUiRetire: vi.fn(async () => {}), nativeUiVisible: vi.fn(async () => {}),
  nativeUiBounds: vi.fn(async () => {}), nativeUiBrowser: vi.fn(async () => {}), onNativeUi: vi.fn(async () => () => {}),
  archivePage: vi.fn(() => new Promise(() => {})),
  resetArchivePages: vi.fn(async () => {}),
  cancelArchivePages: vi.fn(async () => {}),
  runAction: vi.fn(),
  inspectSession: vi.fn(async () => null),
  pendingReadMarks: vi.fn(async () => []),
  attentionMarks: vi.fn(async () => ({ marks: {}, replies: {} })),
  updateAttentionMarks: vi.fn(async () => {}),
  queueReadMarks: vi.fn(async () => []),
  sessionReference: vi.fn(),
  copyCommand: vi.fn(),
  copyText: vi.fn(),
  setNotifications: vi.fn(),
  // The composer and Saved drafts, loaded with the list's Saved drafts button.
  windowVisibility: { hidden: () => false, onChange: () => () => {} },
  composerOpen: vi.fn(async () => null), composerEdit: vi.fn(async () => {}), composerPick: vi.fn(async () => {}), composerPasteImage: vi.fn(async () => {}),
  composerRemove: vi.fn(async () => {}), composerUndoRemove: vi.fn(async () => {}), composerAllowRetry: vi.fn(async () => {}), composerRefresh: vi.fn(async () => {}),
  composerTab: vi.fn(async () => {}), sendMessage: vi.fn(), sendAnswer: vi.fn(), sendCancel: vi.fn(async () => {}), sendInterrupted: vi.fn(async () => {}), clearContext: vi.fn(),
  localRetry: vi.fn(), localDelete: vi.fn(async () => {}), localInspect: vi.fn(async () => null),
  savedDrafts: vi.fn(), draftConflicts: vi.fn(), savedDraft: vi.fn(), draftRecover: vi.fn(), draftRestore: vi.fn(),
  composerClearControl: vi.fn(async () => null),
  sessionFolderStatus: vi.fn(async () => ({ folder: false, shell: false, folder_hint: "Unavailable" })),
  sessionFolderOpen: vi.fn(async () => ""),
  // Content search (store/contentSearch.ts) and the creation cards start with the list.
  inTauri: () => false,
  onCreations: vi.fn(async () => () => {}),
}));

import { sessionContextMenu, sessionMenu } from "./SessionList";
import { archivedSession } from "../../store/archive";
import { applyBatch, toSession } from "../../store/catalog";
import { composer } from "../../store/composer";
import { composerClearControl, composerOpen, copyText, inspectSession, runAction, sessionFolderOpen, sessionFolderStatus, sessionReference } from "../../bridge";
import { detailTab, menu, visibleMenu, multi, peek, selectedKey, setDetailTab, setMenu, setMulti, setPeek, setSelectedKey, setStripMode, setSubagentOpen, subagentOpen, type MenuEntry } from "../../store/ui";
import { clearMulti } from "./organize";
import { setSetting } from "../../store/settings";
import { emptyBatch, type ComposerView, type ViewRow } from "../../types";

const labels = (entries: MenuEntry[]) => entries.map((e) => ("separator" in e ? "---" : "heading" in e ? `# ${e.heading}` : e.label));
const archived = (agent: string): ViewRow => ({
  name: `${agent}/example/a1`, agent, state: "archived", status: "ended", archive_id: "10000000-0000-4000-8000-000000000001",
  archived_at: 100, run_id: "run-a1", conversation_id: "conv-a1", resumable: true,
});

afterEach(() => {
  setMenu(undefined); clearMulti(); setSelectedKey(undefined); setSubagentOpen(undefined);
  setPeek(false); setStripMode(false); setDetailTab("activity"); composer.apply(undefined);
  vi.clearAllMocks();
});

describe("the session menu (Qt showSessionMenu 5266-5382)", () => {
  it("an archived row has no terminal entry and no Mute or Mark group, and no doubled separators", () => {
    const expected = [
      "Compact session row", "Expand all collapsed", "---",
      "Open folder", "Open shell in session folder", "Copy restore command", "---",
      "Restore session", "Rename…", "Fork session…", "Move to project…", "---",
      "Copy session name", "Copy session reference", "Copy folder path", "---",
      "Forget archived session…",
    ];
    expect(labels(sessionMenu(archivedSession("", "Local", archived("codex"))))).toEqual(expected);
    expect(labels(sessionMenu(archivedSession("", "Local", archived("dsh"))))).toEqual(expected);
  });

  it("a live row starts with Mark then Mute and keeps its terminal entry", () => {
    const live = toSession("", "run:run-live", { name: "codex/example/live", agent: "codex", state: "running", status: "ready", run_id: "run-live", conversation_id: "conv-live" });
    expect(labels(sessionMenu(live))).toEqual([
      "Mark needs attention", "Mute", "---",
      "Compact session row", "Expand all collapsed", "---",
      "Open terminal", "Open folder", "Open shell in session folder", "Copy connect command", "---",
      "Pause session", "Clear session…", "Rename…", "Fork session…", "Move to project…", "---",
      "Copy session name", "Copy session reference", "Copy folder path", "---",
      "Terminate session…",
    ]);
  });

  it("Clear session checks an unselected row and opens the same dialog for it", async () => {
    const row: ViewRow = { name: "codex/example/live", agent: "codex", state: "running", status: "ready", run_id: "run-live", conversation_id: "conv-live" };
    applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }], upserts: [{ computer: "", key: "run:run-live", row }] });
    const live = toSession("", "run:run-live", row);
    const clear = { enabled: true, dismiss: false, title: "Clear session?", text: "", confirm: "Clear session", run: "run-live", conversation: "conv-live",
      cache_enabled: true, running: false, reason: "", cache_reason: "" };
    vi.mocked(composerClearControl).mockResolvedValue(clear);
    setSelectedKey(live.key);
    const entry = (s = live) => sessionMenu(s).find((e) => "label" in e && e.label === "Clear session…") as Extract<MenuEntry, { label: string }>;
    expect(entry()).toMatchObject({ disabled: true, hint: "Checking whether this session can be cleared…" });
    await vi.waitFor(() => expect(entry().disabled).toBe(false));
    expect(composerClearControl).toHaveBeenCalledWith({ computer: "", key: "run:run-live", name: "codex/example/live", archive_id: null });
    entry().action?.();
    await vi.waitFor(() => expect(composer.dialog()).toMatchObject({ kind: "clear", control: { run: "run-live" } }));
    expect(selectedKey()).toBe(live.key);
    composer.closeDialog();

    composer.apply({ handle: 1, key: "\ncodex/example/live", loaded: true, text_epoch: 1, text: "", attachments: [], local: [], clear } as unknown as ComposerView);
    entry().action?.();
    expect(composer.dialog()).toMatchObject({ kind: "clear", control: { run: "run-live" } });
    composer.closeDialog();
    // The row paused since the menu opened: nothing opens.
    const stale = entry();
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:run-live", row: { ...row, state: "paused", status: "paused" } }] });
    stale.action?.();
    expect(composer.dialog()).toBeUndefined();
    composer.apply({ handle: 1, key: "\ncodex/example/live", loaded: true, text_epoch: 1, text: "", attachments: [], local: [], clear: { ...clear, enabled: false, reason: "Wait for the session update to finish." } } as unknown as ComposerView);
    setSelectedKey(live.key);
    expect(entry()).toMatchObject({ disabled: true, hint: "Wait for the session update to finish." });
    composer.apply(undefined);
    setSelectedKey(undefined);
  });
});

function contextRows() {
  const row = (tag: string): ViewRow => ({ name: `codex/context/${tag}`, agent: "codex", state: "running", status: "ready", run_id: `context-${tag}`, conversation_id: `conversation-${tag}`, cwd: `/tmp/context-${tag}` });
  const rows = ["open", "target", "other"].map((tag) => ({ computer: "", key: `run:context-${tag}`, row: row(tag) }));
  applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }], upserts: rows });
  return rows.map((r) => toSession(r.computer, r.key, r.row));
}
function contextEvent(overrides: Partial<MouseEvent> = {}): MouseEvent {
  return { button: 2, ctrlKey: false, clientX: 20, clientY: 40, preventDefault: vi.fn(), stopPropagation: vi.fn(), ...overrides } as unknown as MouseEvent;
}
const menuItem = (label: string, items = menu()?.items ?? []) => {
  const item = items.find((e) => "label" in e && e.label === label);
  if (!item || !("label" in item)) throw Error(`Missing menu action: ${label}`);
  return item;
};

describe("session context targeting", () => {
  it("opens within the bound and adopts independent replies while inspection is stalled", async () => {
    vi.useFakeTimers();
    try {
      const row: ViewRow = { name: "codex/context/slow", agent: "codex", state: "running", status: "ready", run_id: "slow-context", conversation_id: "slow-conversation", cwd: "/example/menu" };
      applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }], upserts: [{ computer: "", key: "run:slow-context", row }] });
      const target = toSession("", "run:slow-context", row);
      let folder!: (value: Awaited<ReturnType<typeof sessionFolderStatus>>) => void;
      let cleared!: (value: Awaited<ReturnType<typeof composerClearControl>>) => void;
      vi.mocked(sessionFolderStatus).mockImplementationOnce(() => new Promise((resolve) => { folder = resolve; }));
      vi.mocked(inspectSession).mockImplementationOnce(() => new Promise(() => {}));
      vi.mocked(composerClearControl).mockImplementationOnce(() => new Promise((resolve) => { cleared = resolve; }));
      sessionContextMenu(contextEvent(), target);
      await vi.advanceTimersByTimeAsync(179); expect(visibleMenu()).toBeUndefined();
      await vi.advanceTimersByTimeAsync(1); expect(visibleMenu()?.sessionKey).toBe(target.key);
      expect(menuItem("Open folder").disabled).toBe(true);
      folder({ folder: true, shell: true, folder_hint: "" });
      cleared({ enabled: true, dismiss: false, title: "Clear session?", text: "", confirm: "Clear session", run: row.run_id!, conversation: row.conversation_id!, cache_enabled: true, running: false, reason: "", cache_reason: "" });
      await vi.advanceTimersByTimeAsync(0);
      expect(visibleMenu()).toBe(menu());
      expect(menuItem("Open folder").disabled).toBe(false);
      expect(menuItem("Clear session…").disabled).toBe(false);
      expect(menuItem("Fork session…").disabled).toBe(true);
    } finally { setMenu(undefined); vi.useRealTimers(); }
  });

  it.each([false, true])("publishes the prepared snapshot when reads settle before the bound (failure=%s)", async (failure) => {
    const tag = failure ? "final-failed" : "final-ready";
    const row: ViewRow = { name: `codex/context/${tag}`, agent: "codex", state: "running", status: "ready", run_id: tag, conversation_id: `conversation-${tag}`, cwd: "/example/menu" };
    applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }], upserts: [{ computer: "", key: `run:${tag}`, row }] });
    const target = toSession("", `run:${tag}`, row);
    let folder!: (value: Awaited<ReturnType<typeof sessionFolderStatus>>) => void;
    let inspected!: (value: Awaited<ReturnType<typeof inspectSession>>) => void;
    let cleared!: (value: Awaited<ReturnType<typeof composerClearControl>>) => void;
    vi.mocked(sessionFolderStatus).mockImplementationOnce(() => new Promise((resolve) => { folder = resolve; }));
    vi.mocked(inspectSession).mockImplementationOnce(() => new Promise((resolve) => { inspected = resolve; }));
    vi.mocked(composerClearControl).mockImplementationOnce(() => new Promise((resolve) => { cleared = resolve; }));
    sessionContextMenu(contextEvent(), target);
    const initial = menu();
    expect(initial?.sessionKey).toBe(target.key); expect(visibleMenu()).toBeUndefined();
    folder({ folder: !failure, shell: !failure, folder_hint: "" });
    await Promise.resolve(); await Promise.resolve();
    expect(menu()?.sessionKey).toBe(initial?.sessionKey); expect(visibleMenu()).toBeUndefined();
    inspected(null);
    await Promise.resolve(); await Promise.resolve();
    expect(menu()?.sessionKey).toBe(initial?.sessionKey); expect(visibleMenu()).toBeUndefined();
    cleared(failure ? null : { enabled: true, dismiss: false, title: "Clear session?", text: "", confirm: "Clear session", run: tag, conversation: row.conversation_id!, cache_enabled: true, running: false, reason: "", cache_reason: "" });
    await vi.waitFor(() => expect(visibleMenu()?.sessionKey).toBe(target.key));
    const final = visibleMenu();
    expect(menuItem("Open folder").disabled).toBe(failure);
    expect(menuItem("Clear session…").disabled).toBe(failure);
    await Promise.resolve(); expect(visibleMenu()).toBe(final);
    expect(composerClearControl).toHaveBeenCalledTimes(1); // A failed preparation is not retried while building the final entries.
  });

  it.each([false, true])("opening and closing a context menu preserves the conversation, draft and selection (strip=%s)", async (strip) => {
    const [open, target] = contextRows();
    setStripMode(strip); setSelectedKey(open.key); setSubagentOpen("open-child");
    setMulti(open.key, true); setPeek(true); setDetailTab("terminal");
    composer.apply({ handle: 17, key: open.key, loaded: true, text_epoch: 1, text: "unsent draft", attachments: [], local: [] } as unknown as ComposerView);
    const draft = composer.view();
    sessionContextMenu(contextEvent(), target);
    expect(menu()?.sessionKey).toBe(target.key);
    expect(selectedKey()).toBe(open.key); expect(subagentOpen()).toBe("open-child");
    expect(multi[open.key]).toBe(true); expect(multi[target.key]).not.toBe(true);
    expect(peek()).toBe(true); expect(detailTab()).toBe("terminal");
    expect(composer.view()).toBe(draft); expect(composerOpen).not.toHaveBeenCalled();
    setMenu(undefined);
    await Promise.resolve(); await Promise.resolve();
    expect(menu()).toBeUndefined(); // Late capability reads cannot restore the highlight.
    expect(selectedKey()).toBe(open.key); expect(composer.view()).toBe(draft);
  });

  it("keeps menu actions bound to the context session while another conversation stays open", async () => {
    const [open, target] = contextRows(); setSelectedKey(open.key);
    vi.mocked(sessionFolderStatus).mockResolvedValueOnce({ folder: true, shell: true, folder_hint: "" });
    vi.mocked(runAction).mockResolvedValueOnce({ outcome: "done", receipt: null, cleanup: null });
    vi.mocked(sessionReference).mockResolvedValueOnce("zerus://session?name=context-target");
    sessionContextMenu(contextEvent(), target);
    await vi.waitFor(() => expect(menuItem("Open folder").disabled).toBe(false));
    const items = menu()!.items; setMenu(undefined); // MenuLayer closes before running an action.
    menuItem("Copy session name", items).action?.();
    expect(copyText).toHaveBeenCalledWith(target.name);
    menuItem("Copy session reference", items).action?.();
    const nativeTarget = { computer: target.computer, name: target.name, run_id: target.runId, conversation_id: target.conversationId };
    expect(sessionReference).toHaveBeenCalledWith(nativeTarget, null);
    menuItem("Open folder", items).action?.();
    expect(sessionFolderOpen).toHaveBeenCalledWith({ target: nativeTarget, archive_id: null, agent_id: null, directory: target.cwdPath }, "folder");
    menuItem("Pause session", items).action?.();
    await vi.waitFor(() => expect(runAction).toHaveBeenCalledWith(nativeTarget, { kind: "pause" }));
    expect(selectedKey()).toBe(open.key); expect(composerOpen).not.toHaveBeenCalled();
  });

  it("does not let an old folder result retarget a replacement or closed menu", async () => {
    const [open, target, other] = contextRows(); setSelectedKey(open.key);
    let finish!: (value: { folder: boolean; shell: boolean; folder_hint: string }) => void;
    vi.mocked(sessionFolderStatus).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    sessionContextMenu(contextEvent(), target);
    sessionContextMenu(contextEvent(), other);
    finish({ folder: true, shell: true, folder_hint: "" });
    await Promise.resolve(); await Promise.resolve();
    expect(menu()?.sessionKey).toBe(other.key);
    expect(menuItem("Open folder").disabled).toBe(true);
    setMenu(undefined); await Promise.resolve();
    expect(menu()).toBeUndefined(); expect(selectedKey()).toBe(open.key);
  });

  it("opens running commands for the context session instead of the current subagent", async () => {
    const [open, target] = contextRows();
    setSelectedKey(open.key); setSubagentOpen("open-child");
    await setSetting("processes/enabled", true);
    try {
      sessionContextMenu(contextEvent(), { ...target, phase: "idle", activity: "idle", backgroundJobs: 1 });
      const item = menuItem("Show running commands");
      expect(item.disabled).toBe(false); expect(selectedKey()).toBe(open.key);
      setMenu(undefined); item.action?.();
      expect(selectedKey()).toBe(target.key); expect(subagentOpen()).toBeUndefined();
      expect(detailTab()).toBe("processes");
    } finally { await setSetting("processes/enabled", false); }
  });

  it("keeps macOS Control-click as additive selection without a context menu", () => {
    const [open, target] = contextRows(); setSelectedKey(open.key);
    const event = contextEvent({ button: 0, ctrlKey: true });
    sessionContextMenu(event, target);
    expect(event.stopPropagation).toHaveBeenCalled(); // Do not open the list background menu.
    expect(menu()).toBeUndefined(); expect(selectedKey()).toBe(open.key);
    expect(multi[open.key]).toBe(true); expect(multi[target.key]).toBe(true);
    expect(sessionFolderStatus).not.toHaveBeenCalled(); expect(inspectSession).not.toHaveBeenCalled();
  });

  it.each([false, true])("preserves the existing multi-selection menu (strip=%s)", (strip) => {
    const [open, target] = contextRows(); setStripMode(strip); setSelectedKey(open.key);
    setMulti(open.key, true); setMulti(target.key, true);
    sessionContextMenu(contextEvent(), target);
    expect(menu()?.items[0]).toEqual({ heading: "2 selected sessions" });
    expect(menu()?.sessionKey).toBeUndefined();
    expect(multi[open.key]).toBe(true); expect(multi[target.key]).toBe(true);
    expect(selectedKey()).toBe(open.key); expect(sessionFolderStatus).not.toHaveBeenCalled();
  });
});
