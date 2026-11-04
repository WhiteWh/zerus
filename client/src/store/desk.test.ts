import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as bridge from "../bridge";
import * as ui from "./ui";
import * as organization from "./organization";
import * as settingsSections from "../ui/screens/settings/sections";
import { openSession, startDesk } from "./desk";
import { catalog } from "./catalog";
import { listedSessionIgnoringCollapse } from "../ui/sessions/model";

vi.mock("../ui/screens/settings/sections", () => ({ openSettings: vi.fn() }));
vi.mock("../bridge", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../bridge")>()),
  onOpenSession: vi.fn(async () => () => {}),
  onOpenUpdates: vi.fn(async () => () => {}),
  onDeskNotice: vi.fn(async () => () => {}),
  deskPage: vi.fn(async () => 3),
  deskTakePending: vi.fn(async () => ({ open: null, notices: [] })),
}));
// One catalog row, in a project the list shows collapsed.
vi.mock("./catalog", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./catalog")>()),
  catalog: { rows: { "mac\nrun:folded": { project: "sample-project" } }, order: [], computers: {} },
}));
// The organization groups that row under a collapsed project; the Rust side keeps the collapse.
vi.mock("./organization", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./organization")>()),
  groupOf: vi.fn((id: string) => (id === "mac\nrun:folded" ? "folded-project" : "ungrouped")),
  groups: { "mac\nrun:folded": { group: "folded-project", rank: 0 } },
  defaultProject: vi.fn(() => "ungrouped"),
  grouping: vi.fn(() => ({ loaded: true, headers: [{ id: "folded-project", collapsed: true }] })),
  headerOf: vi.fn((project: string) => (project === "folded-project" ? { id: project, collapsed: true } : undefined)),
  setProjectCollapsed: vi.fn(),
}));
// store/ui reads the page URL when it loads; the wiring needs only its setters.
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });
vi.mock("./ui", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./ui")>()),
  stateFilter: vi.fn(() => "all"),
  machineFilter: vi.fn(() => []),
  projectFilter: vi.fn(() => ["folded-project"]),
  search: vi.fn(() => ""),
  page: vi.fn(() => "sessions"),
  selectedKey: vi.fn(() => undefined),
  setPage: vi.fn(),
  setSelectedKey: vi.fn(),
  setSubagentOpen: vi.fn(),
  setStateFilter: vi.fn(),
  setMachineFilter: vi.fn(),
  setProjectFilter: vi.fn(),
  setSearch: vi.fn(),
  setDetailTab: vi.fn(),
  revealRow: vi.fn(),
  toast: vi.fn(),
}));
const mocked = vi.mocked(bridge);
const shown = vi.mocked(ui);
const projects = vi.mocked(organization);
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Starts the wiring and returns the open-session listener it registered. */
async function opener(): Promise<(target: bridge.OpenSession) => void> {
  let open: ((target: bridge.OpenSession) => void) | undefined;
  mocked.onOpenSession.mockImplementation(async (listener) => {
    open = listener;
    return () => {};
  });
  await startDesk();
  return open!;
}

describe("desk wiring", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(ui.page).mockReturnValue("sessions");
    vi.mocked(ui.selectedKey).mockReturnValue(undefined);
    delete catalog.rows["peer\nrun:exact"];
    vi.mocked(ui.search).mockReturnValue("");
    vi.mocked(ui.projectFilter).mockReturnValue(["folded-project"]);
  });

  afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });

  it.each([false, true])("keeps an exact cold-view open until its original identity arrives (stale row: %s)", (stale) => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date", "performance"] });
    if (stale) Object.assign(catalog.rows, { "peer\nrun:exact": { runId: "older", conversationId: "older", state: "running" } });
    openSession({ computer: "peer", key: "run:exact", attention: true, expected: { run: "r", conversation: "c" } });
    expect(shown.setSelectedKey).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(1);
    Object.assign(catalog.rows, { "peer\nrun:exact": { runId: "r", conversationId: "c", state: "running" } });
    vi.advanceTimersByTime(50);
    expect(shown.setSelectedKey).toHaveBeenCalledWith("peer\nrun:exact");
    expect(shown.setDetailTab).toHaveBeenCalledWith("activity");
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each([
    { runId: "different", conversationId: "c", state: "running" },
    { runId: "r", conversationId: "different", state: "running" },
    { runId: "r", conversationId: "c", state: "archived" },
  ])("refuses a key now bound to another or archived session: %j", (row) => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date", "performance"] });
    Object.assign(catalog.rows, { "peer\nrun:exact": row });
    openSession({ computer: "peer", key: "run:exact", attention: true, expected: { run: "r", conversation: "c" } });
    expect(shown.setSelectedKey).not.toHaveBeenCalled();
    expect(shown.setDetailTab).not.toHaveBeenCalled();
    vi.advanceTimersByTime(16_000);
    expect(shown.setSelectedKey).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("bounds a missing exact open and supersedes it with explicit user navigation", () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date", "performance"] });
    const target = { computer: "peer", key: "run:exact", expected: { run: "r", conversation: "c" } };
    openSession(target);
    vi.advanceTimersByTime(16_000);
    expect(shown.setSelectedKey).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
    openSession(target);
    openSession({ computer: "mac", key: "manual" });
    Object.assign(catalog.rows, { "peer\nrun:exact": { runId: "r", conversationId: "c", state: "running" } });
    vi.advanceTimersByTime(100);
    expect(shown.setSelectedKey).toHaveBeenCalledTimes(1);
    expect(shown.setSelectedKey).toHaveBeenCalledWith("mac\nmanual");
  });

  it("refuses an exact row delivered by a timer that ran after its original deadline", () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date", "performance"] });
    const clock = vi.spyOn(performance, "now").mockReturnValue(0);
    try {
      openSession({ computer: "peer", key: "run:exact", expected: { run: "r", conversation: "c" } });
      Object.assign(catalog.rows, { "peer\nrun:exact": { runId: "r", conversationId: "c", state: "running" } });
      clock.mockReturnValue(20_000);
      vi.advanceTimersByTime(50);
      expect(shown.setSelectedKey).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(0);
    } finally { clock.mockRestore(); }
  });

  it("does not snap back after the user changes the page while the exact row loads", () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date", "performance"] });
    openSession({ computer: "peer", key: "run:exact", expected: { run: "r", conversation: "c" } });
    vi.mocked(ui.page).mockReturnValue("settings");
    Object.assign(catalog.rows, { "peer\nrun:exact": { runId: "r", conversationId: "c", state: "running" } });
    vi.advanceTimersByTime(100);
    expect(shown.setSelectedKey).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("opens the session the menu bar or a notification names", async () => {
    const stop = vi.fn();
    let open: ((target: bridge.OpenSession) => void) | undefined;
    mocked.onOpenSession.mockImplementation(async (listener) => {
      open = listener;
      return stop;
    });
    const stopDesk = await startDesk();
    open!({ computer: "mac", key: "claude/sample-project/review" });
    expect(shown.setPage).toHaveBeenCalledWith("sessions");
    expect(shown.setSelectedKey).toHaveBeenCalledWith("mac\nclaude/sample-project/review");
    expect(shown.revealRow).toHaveBeenCalledWith("mac\nclaude/sample-project/review");
    stopDesk();
    expect(stop).toHaveBeenCalled();
  });

  it("opens a clicked notification's session on Activity, a menu row on the tab it had", async () => {
    const open = await opener();
    open({ computer: "mac", key: "run:folded" });
    expect(shown.setDetailTab).not.toHaveBeenCalled();
    vi.clearAllMocks();
    open({ computer: "mac", key: "run:folded", attention: true });
    expect(shown.setDetailTab).toHaveBeenCalledWith("activity");
    // The click expands the row's organization group as a menu row does.
    expect(projects.setProjectCollapsed).toHaveBeenCalledWith("folded-project", false);
    expect(shown.setSelectedKey).toHaveBeenLastCalledWith("mac\nrun:folded");
  });

  it("clears every filter first, as Qt's showSession does", async () => {
    const open = await opener();
    open({ computer: "", key: "k1" });
    expect(shown.setStateFilter).toHaveBeenCalledWith("all");
    expect(shown.setMachineFilter).toHaveBeenCalledWith([]);
    expect(shown.setProjectFilter).toHaveBeenCalledWith([]);
    expect(shown.setSearch).toHaveBeenCalledWith("");
    expect(shown.setSubagentOpen).toHaveBeenCalledWith(undefined);
  });

  it("keeps an active filter while revealing an owner in its collapsed project", () => {
    expect(listedSessionIgnoringCollapse("mac\nrun:folded")).toBe(true);
    openSession(
      { computer: "mac", key: "run:folded", preserveFilters: true },
      { keepFiltersIf: listedSessionIgnoringCollapse },
    );
    expect(projects.setProjectCollapsed).toHaveBeenCalledWith("folded-project", false);
    expect(shown.setStateFilter).not.toHaveBeenCalled();
    expect(shown.setMachineFilter).not.toHaveBeenCalled();
    expect(shown.setProjectFilter).not.toHaveBeenCalled();
    expect(shown.setSearch).not.toHaveBeenCalled();
    vi.clearAllMocks();
    vi.mocked(ui.projectFilter).mockReturnValue(["another-project"]);
    expect(listedSessionIgnoringCollapse("mac\nrun:folded")).toBe(false);
    openSession(
      { computer: "mac", key: "run:folded", preserveFilters: true },
      { keepFiltersIf: listedSessionIgnoringCollapse },
    );
    expect(shown.setStateFilter).toHaveBeenCalledWith("all");
    expect(shown.setMachineFilter).toHaveBeenCalledWith([]);
    expect(shown.setProjectFilter).toHaveBeenCalledWith([]);
    expect(shown.setSearch).toHaveBeenCalledWith("");
    vi.clearAllMocks();
    vi.mocked(ui.search).mockReturnValue("draft");
    const listed = vi.fn(() => true);
    openSession({ computer: "mac", key: "run:folded", preserveFilters: true }, { keepFiltersIf: listed });
    expect(listed).not.toHaveBeenCalled();
    expect(shown.setStateFilter).toHaveBeenCalledWith("all");
    expect(shown.setSearch).toHaveBeenCalledWith("");
  });

  it("expands the collapsed project of the session it opens", async () => {
    const open = await opener();
    open({ computer: "mac", key: "run:folded" });
    expect(projects.setProjectCollapsed).toHaveBeenCalledWith("folded-project", false);
    expect(shown.setSelectedKey).toHaveBeenCalledWith("mac\nrun:folded");
    vi.clearAllMocks();
    open({ computer: "mac", key: "run:elsewhere" });
    expect(projects.setProjectCollapsed).not.toHaveBeenCalled();
  });

  it("takes what waited only once both listeners are registered", async () => {
    let listening!: (stop: () => void) => void;
    mocked.onDeskNotice.mockImplementationOnce(() => new Promise((resolve) => { listening = resolve; }));
    const started = startDesk();
    await settle();
    expect(mocked.deskPage).not.toHaveBeenCalled();
    expect(mocked.deskTakePending).not.toHaveBeenCalled();
    listening(() => {});
    await started;
    await settle();
    expect(mocked.deskTakePending).toHaveBeenCalledWith(3);
  });

  it("applies what arrived before the page loaded", async () => {
    mocked.deskTakePending.mockResolvedValueOnce({
      open: { computer: "", key: "k1" },
      notices: [{ title: "Zerus", text: "This session is no longer available: claude/sample-project/x" }],
    });
    await startDesk();
    await settle();
    expect(mocked.deskTakePending).toHaveBeenCalledWith(3);
    expect(shown.setSelectedKey).toHaveBeenCalledWith("\nk1");
    expect(shown.toast).toHaveBeenCalledWith("Zerus", "This session is no longer available: claude/sample-project/x", "info", false);
  });

  it("ignores an original pending reply or queued activation after the desk stops", async () => {
    let reply!: (pending: bridge.DeskPending) => void;
    let clicked!: (target: bridge.OpenSession) => void;
    mocked.deskTakePending.mockImplementationOnce(() => new Promise((resolve) => { reply = resolve; }));
    mocked.onOpenSession.mockImplementationOnce(async (listener) => { clicked = listener; return () => {}; });
    const stop = await startDesk();
    await settle();
    stop();
    Object.assign(catalog.rows, { "peer\nrun:exact": { runId: "r", conversationId: "c", state: "running" } });
    const target = { computer: "peer", key: "run:exact", expected: { run: "r", conversation: "c" } };
    clicked(target);
    reply({ open: target, notices: [] });
    await settle();
    expect(shown.setSelectedKey).not.toHaveBeenCalled();
  });

  it("shows notices as toasts", async () => {
    let notice: ((value: bridge.DeskNotice) => void) | undefined;
    mocked.onDeskNotice.mockImplementation(async (listener) => {
      notice = listener;
      return () => {};
    });
    await startDesk();
    notice!({ title: "Zerus", text: "System notification was not delivered: denied" });
    expect(shown.toast).toHaveBeenCalledWith("Zerus", "System notification was not delivered: denied", "info", false);
  });

  it("keeps an advisory with what to do by hand until it is dismissed", async () => {
    let notice: ((value: bridge.DeskNotice) => void) | undefined;
    mocked.onDeskNotice.mockImplementation(async (listener) => {
      notice = listener;
      return () => {};
    });
    await startDesk();
    notice!({ title: "Zerus: the terminal did not open", text: "Run this in a terminal: hgs a example" });
    expect(shown.toast).toHaveBeenCalledWith("Zerus: the terminal did not open", "Run this in a terminal: hgs a example", "warn", true);
  });

  it("opens an update action in Settings, never as a session", async () => {
    let open!: () => void;
    mocked.onOpenUpdates.mockImplementation(async (listener) => {
      open = listener;
      return () => {};
    });
    await startDesk();
    open();
    await settle();
    expect(settingsSections.openSettings).toHaveBeenCalledWith("updates");
    expect(shown.setSelectedKey).not.toHaveBeenCalled();
  });

  it("applies Updates that waited for this page", async () => {
    mocked.deskTakePending.mockResolvedValueOnce({ open: null, notices: [], updates: true });
    await startDesk();
    await settle();
    expect(settingsSections.openSettings).toHaveBeenCalledWith("updates");
    expect(shown.setPage).not.toHaveBeenCalledWith("sessions");
  });
});
