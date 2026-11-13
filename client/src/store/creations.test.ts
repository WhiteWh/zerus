import { describe, expect, it, vi } from "vitest";
import { createRoot, createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import { createCreations, type CreationsDeps } from "./creations";
import { createNoticeBus, ERROR_NOTICE_MS, type NoticeBus } from "./notices";
import { toSession } from "./catalog";
import { RESOLVED, creationRowId, type CardScope, type ListScope } from "../ui/sessions/creations-ui";
import type { CreationCard, CreationEvent, GroupingOut, ViewRow } from "../types";
import type { Session } from "../ui/sessions/types";
import type { Row } from "../ui/sessions/model";

// The creation cards' store over a recorded bridge: selection, reveal and
// adoption (SessionsWindow.cpp 3020-3075, 3253-3255, 3434-3442, 3717-3728),
// the status menu (463-480), the admission (2911-2919), Edit choices…
// (1525-1528) and the notices (1571-1586).

// The store's module reads the page's query (store/ui.ts); the tests run without a page.
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });

const card = (id: string, extra: Partial<CreationCard> = {}): CreationCard => ({
  id, tag: `tag-${id}`, state: "queued", caption: "Queued", error: "", created: 1_793_000_000, group: "alpha", host: "",
  machine: "workstation", cmd: "claude", account: "", target: "/workspace/example", show_pending: true, launch_dispatched: false,
  stop_requested: false, worktree_receipts: false, can_resume: false, can_edit: false, bound_session: "", ...extra,
});
const header = (id: string, collapsed = false) => ({ id, name: id.toUpperCase(), color: null, accessible: true, collapsed, is_default: id === "ungrouped", palette: 0 });
const grouping = (headers: ReturnType<typeof header>[]): GroupingOut => ({
  loaded: true, default_project: "ungrouped", headers, ready: true, reason: null, busy: false, status: "", status_error: false, conflicts: 0,
});
type MenuEntryLike = { label: string; action?: () => void };
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };

/** The commands a card may send; anything else (a composer, a draft) fails the test. */
const CREATION_COMMANDS = ["onCreations", "creationsView", "creationDetails", "creationAction", "creationPlan"];

function setup(view: CreationCard[] = [], options: { bus?: NoticeBus; grouping?: GroupingOut; plans?: Record<string, Record<string, unknown> | null>; details?: Record<string, string>; refuse?: string } = {}) {
  return createRoot((dispose) => {
    const calls: { name: string; args: unknown[] }[] = [];
    let listener: ((e: CreationEvent) => void) | undefined;
    const answers: Record<string, (...args: never[]) => unknown> = {
      onCreations: (l: (e: CreationEvent) => void) => { listener = l; return Promise.resolve(() => {}); },
      creationsView: () => Promise.resolve(view),
      creationDetails: (id: string) => Promise.resolve(options.details?.[id] ?? null),
      creationAction: () => (options.refuse ? Promise.reject(options.refuse) : Promise.resolve()),
      creationPlan: (id: string) => Promise.resolve(options.plans?.[id] ?? null),
    };
    // Every command the store sends is recorded, whatever its name.
    const bridge = new Proxy({}, {
      get: (_target, name: string) => (...args: unknown[]) => {
        calls.push({ name, args });
        const answer = answers[name] as ((...a: unknown[]) => unknown) | undefined;
        return answer ? answer(...args) : Promise.reject(`unexpected ${name}`);
      },
    }) as CreationsDeps["bridge"];
    const [selectedKey, setSelectedKey] = createSignal<string>();
    const [subagent, setSubagent] = createSignal<string | undefined>("child-1");
    const [group] = createSignal(options.grouping ?? grouping([header("alpha"), header("ungrouped")]));
    const [scope, setScope] = createSignal<Omit<CardScope, "grouping">>({ filter: "all", hosts: [], query: "", projects: [] });
    const [order, setOrder] = createSignal<string[]>([]);
    const [online, setOnline] = createSignal(true);
    const [rows, setRows] = createStore<Record<string, Session>>({});
    const notices: { tone: string; text: string; sticky?: boolean }[] = [];
    const opened: unknown[] = [];
    const revealed: string[] = [];
    const collapsed: [string, boolean][] = [];
    let page = "dashboard";
    const store = createCreations({
      bridge,
      notify: (tone, text, sticky) => { notices.push({ tone, text, sticky }); options.bus?.notify(tone, text, sticky); },
      openNewSession: async (o) => { opened.push(o); },
      selectedKey, setSelectedKey,
      closeSubagent: () => setSubagent(undefined),
      showSessions: () => { page = "sessions"; },
      reveal: (key) => revealed.push(key),
      grouping: group,
      groupOf: () => "alpha",
      setProjectCollapsed: (p, c) => collapsed.push([p, c]),
      session: (key) => rows[key],
      sessionKeys: order,
      online,
      scope: () => ({ grouping: group(), ...scope() }),
      filterKey: () => JSON.stringify(scope()),
    });
    const list = (n: number, extra: Partial<ViewRow> = {}, computer = "") => {
      const s = toSession(computer, `run:${n}`, { name: `claude/sample-project/s${n}`, agent: "claude", status: "ready", ...extra });
      setRows(s.key, s);
      setOrder((o) => o.includes(s.key) ? o : [...o, s.key]);
      return s;
    };
    return {
      store, calls, notices, opened, revealed, collapsed, selectedKey, setSelectedKey, subagent, setScope, list, setOnline, dispose,
      page: () => page,
      emit: (e: CreationEvent) => listener!(e),
      started: store.start(),
    };
  });
}

describe("the cards", () => {
  const scope = (): ListScope => ({ grouping: grouping([header("alpha")]), members: new Map(), filter: "all", hosts: [], query: "", projects: [], accounts: [], folder: undefined,
    visible: () => true, online: () => true, expanded: () => false, compact: () => false });
  const sessionRow = (s: Session): Row => ({ type: "card", id: s.key, session: s, expandable: false, expanded: false, compactRow: false });

  it.each(["", "workstation", "studio"])("replaces the pending row atomically before the bound event (machine %s)", async (machine) => {
    const host = machine === "studio" ? "studio" : "";
    const c = card("a", { host, machine, launch_id: "current-attempt", launch_dispatched: true, state: "confirming" });
    const t = setup([c]);
    await t.started;
    t.store.selectCreation(c.id);
    expect(t.store.withCreations([], scope()).map((r) => r.type)).toEqual(["creation"]);
    const s = t.list(1, { name: "claude/sample/tag-a", launch_id: c.launch_id, machine, state: "running" }, host);
    // No bound event and no async wait: the catalog publication replaces the
    // placeholder and its count in the same render, including the local "" key.
    expect(t.store.withCreations([sessionRow(s)], scope()).map((r) => r.id)).toEqual([s.key]);
    expect(t.store.creationCount()).toBe(0);
    await flush();
    expect(t.selectedKey()).toBe(s.key);
    expect(t.store.creations()).toHaveLength(1); // The host still owns completion.
    t.emit({ ...c, state: "bound", bound_session: s.name });
    expect(t.store.withCreations([sessionRow(s)], scope()).map((r) => r.id)).toEqual([s.key]);
    expect(t.selectedKey()).toBe(s.key);
    t.dispose();
  });

  it.each([
    { row: { launch_id: "retired-attempt" } },
    { computer: "studio" },
    { row: { agent: "codex" } },
    { row: { name: "claude/sample/another-tag" } },
    { row: { state: "archived" } },
    { offline: true },
    { dispatched: false },
    { missing: true },
  ])("keeps a placeholder without an exact current online launch: %j", async ({ row, computer, offline, dispatched, missing }) => {
    const c = card("a", { launch_id: missing ? undefined : "current-attempt", launch_dispatched: dispatched !== false, state: "confirming" });
    const t = setup([c]);
    await t.started;
    t.setOnline(!offline);
    const s = t.list(1, { name: "claude/sample/tag-a", launch_id: "current-attempt", state: "running", ...row }, computer);
    expect(t.store.withCreations([sessionRow(s)], scope()).map((r) => r.type)).toEqual(["card", "creation"]);
    expect(t.store.creationCount()).toBe(1);
    t.dispose();
  });

  it("uses the original provider for a dsh launch despite the DeepSeek display name", async () => {
    const t = setup([card("a", { cmd: "dsh", launch_id: "current", launch_dispatched: true })]);
    await t.started;
    const s = t.list(1, { name: "dsh/sample/tag-a", agent: "deepseek", account_provider: "dsh", launch_id: "current", state: "running" });
    expect(t.store.withCreations([sessionRow(s)], scope()).map((r) => r.id)).toEqual([s.key]);
    t.dispose();
  });

  it("reconciles an existing catalog row updated to the current launch attempt", async () => {
    const t = setup([card("a", { launch_id: "current", launch_dispatched: true })]);
    await t.started;
    t.list(1, { name: "claude/sample/tag-a", launch_id: "retired", state: "running" });
    expect(t.store.creationCount()).toBe(1);
    t.list(1, { name: "claude/sample/tag-a", launch_id: "current", state: "running" });
    expect(t.store.creationCount()).toBe(0);
    t.dispose();
  });

  it("keeps the current placeholder until its row arrives when the bound event wins the race", async () => {
    const c = card("a", { launch_id: "current", launch_dispatched: true, state: "confirming" });
    const t = setup([c]);
    await t.started;
    t.store.selectCreation(c.id);
    t.emit({ ...c, state: "bound", show_pending: false, bound_session: "claude/sample/tag-a" });
    expect(t.store.withCreations([], scope()).map((r) => r.type)).toEqual(["creation"]);
    expect(t.selectedKey()).toBe(creationRowId(c.id));
    const s = t.list(1, { name: "claude/sample/tag-a", launch_id: "current", state: "running" });
    expect(t.store.withCreations([sessionRow(s)], scope()).map((r) => r.id)).toEqual([s.key]);
    expect(t.selectedKey()).toBe(s.key);
    expect(t.store.creations()).toEqual([]);
    t.dispose();
  });

  it.each(["", "studio"])("never adopts another launch with the bound session's name (computer %s)", async (host) => {
    const c = card("a", { host, launch_id: "current", launch_dispatched: true, state: "confirming" });
    const t = setup([c]);
    await t.started;
    t.store.selectCreation(c.id);
    const other = t.list(1, { name: "claude/sample/tag-a", launch_id: "retired", state: "running" }, host);
    t.emit({ ...c, state: "bound", show_pending: false, bound_session: other.name });
    expect(t.selectedKey()).toBe(creationRowId(c.id));
    expect(t.store.creationCount()).toBe(1);
    expect(t.revealed).not.toContain(other.key);
    const current = t.list(2, { name: other.name, launch_id: "current", state: "running" }, host);
    await flush();
    expect(t.selectedKey()).toBe(current.key);
    expect(t.store.creations()).toEqual([]);
    expect(t.revealed).not.toContain(other.key);
    t.dispose();
  });

  it("come from the first read and the events, the events winning", async () => {
    const t = setup([card("a", { caption: "Queued" }), card("b"), card("gone"), card("old", { state: "bound" })]);
    // Before the read answers: a change of a, the removal of gone.
    t.emit(card("a", { state: "checking", caption: "Checking folder…" }));
    t.emit({ id: "gone", removed: true });
    await t.started;
    expect(t.store.creations().map((c) => [c.id, c.caption])).toEqual([["a", "Checking folder…"], ["b", "Queued"]]);
    t.dispose();
  });

  it("keep at most 128, newest first", async () => {
    const t = setup();
    await t.started;
    for (let i = 0; i < 130; i++) t.emit(card(`c${String(i).padStart(3, "0")}`, { created: 1000 + i }));
    expect(t.store.creations()).toHaveLength(128);
    expect(t.store.creations()[0].id).toBe("c129");
    expect(t.store.creations().at(-1)!.id).toBe("c002");
    t.dispose();
  });
});

describe("selection (3434-3442, 3717-3728)", () => {
  it("a card's selection replaces the session's and closes the subagent; a session's replaces the card's", async () => {
    const t = setup([card("a")]);
    await t.started;
    t.setSelectedKey("\nrun:1");
    t.store.selectCreation("a");
    expect(t.selectedKey()).toBe(creationRowId("a"));
    expect(t.store.selectedCreation()).toBe("a");
    expect(t.store.selectedCard()?.tag).toBe("tag-a");
    expect(t.subagent()).toBeUndefined();
    t.setSelectedKey("\nrun:1");
    expect(t.store.selectedCreation()).toBeUndefined();
    // A card this host does not hold is not selected.
    t.store.selectCreation("other");
    expect(t.selectedKey()).toBe("\nrun:1");
    t.dispose();
  });

  it("a hidden card loses its selection and reveal", async () => {
    const t = setup([card("a")]);
    await t.started;
    t.store.choose("a");
    await flush();
    expect(t.store.revealed()).toBe("a");
    t.emit(card("a", { show_pending: false }));
    expect(t.store.selectedCreation()).toBeUndefined();
    expect(t.store.revealed()).toBeUndefined();
    t.dispose();
  });

  it("a bound card hands its selection to its exact session, which shows through the filters until they change", async () => {
    const t = setup([card("a", { tag: "s7", host: "studio", cmd: "codex" })]);
    await t.started;
    // The same tag in another project is not the host's bound full name.
    t.list(6, { name: "codex/other-project/s7", agent: "codex" }, "studio");
    const session = t.list(7, { name: "codex/sample-project/s7", agent: "codex" }, "studio");
    t.list(8, { name: "codex/sample-project/s7", agent: "codex" });
    t.store.selectCreation("a");
    t.emit(card("a", { tag: "s7", host: "studio", cmd: "codex", state: "bound", show_pending: false, bound_session: "codex/sample-project/s7" }));
    expect(t.selectedKey()).toBe(session.key);
    expect(t.store.adopted()).toBe(session.key);
    expect(t.revealed).toContain(session.key);
    expect(t.store.creations()).toEqual([]);
    // Selecting the adopted row keeps it; a filter change ends it.
    t.setSelectedKey(session.key);
    expect(t.store.adopted()).toBe(session.key);
    t.setScope({ filter: "active", hosts: [], query: "", projects: [] });
    expect(t.store.adopted()).toBeUndefined();
    t.dispose();
  });

  it("a bound card's session that is not listed yet is adopted once it is, unless another row was chosen", async () => {
    const t = setup([card("a", { tag: "s9" }), card("b", { tag: "s10" })]);
    await t.started;
    t.store.selectCreation("a");
    t.emit(card("a", { tag: "s9", state: "bound", show_pending: false, bound_session: "claude/sample-project/s9" }));
    expect(t.selectedKey()).toBeUndefined();
    const session = t.list(9);
    expect(t.selectedKey()).toBe(session.key);
    expect(t.store.adopted()).toBe(session.key);
    t.store.selectCreation("b");
    t.emit(card("b", { tag: "s10", state: "bound", show_pending: false, bound_session: "claude/sample-project/s10" }));
    t.setSelectedKey("\nrun:1");
    t.list(10);
    expect(t.selectedKey()).toBe("\nrun:1");
    t.dispose();
  });

  it("waits for the exact bound row to be online before adopting it", async () => {
    const t = setup([card("a")]);
    await t.started;
    const session = t.list(9);
    t.setOnline(false);
    t.store.selectCreation("a");
    t.emit(card("a", { state: "bound", show_pending: false, bound_session: session.name }));
    expect(t.selectedKey()).toBeUndefined();
    expect(t.store.adopted()).toBeUndefined();
    t.setOnline(true);
    expect(t.selectedKey()).toBe(session.key);
    expect(t.store.adopted()).toBe(session.key);
    t.dispose();
  });

  it("an older host's empty bound name adopts nothing", async () => {
    const t = setup([card("a")]);
    await t.started;
    t.list(9);
    t.store.selectCreation("a");
    t.emit(card("a", { state: "bound", show_pending: false }));
    expect(t.selectedKey()).toBeUndefined();
    expect(t.store.adopted()).toBeUndefined();
    t.dispose();
  });

  for (const end of ["another card", "another session", "a filter", "a search", "Return to filters"] as const) {
    it(`a pending adoption ends on ${end}`, async () => {
      const t = setup([card("a"), card("b")]);
      await t.started;
      t.store.selectCreation("a");
      t.emit(card("a", { state: "bound", show_pending: false, bound_session: "claude/sample-project/s9" }));
      if (end === "another card") t.store.selectCreation("b");
      if (end === "another session") t.setSelectedKey("\nrun:1");
      if (end === "a filter") t.setScope({ filter: "active", hosts: [], query: "", projects: [] });
      if (end === "a search") t.setScope({ filter: "all", hosts: [], query: "x", projects: [] });
      if (end === "Return to filters") t.store.returnToFilters();
      const arrived = t.list(9);
      expect(t.selectedKey()).not.toBe(arrived.key);
      expect(t.store.adopted()).toBeUndefined();
      t.dispose();
    });
  }

  it("an unselected bound card selects nothing", async () => {
    const t = setup([card("a", { tag: "s3" })]);
    await t.started;
    t.list(3);
    t.emit(card("a", { tag: "s3", state: "bound", show_pending: false, bound_session: "claude/sample-project/s3" }));
    expect(t.selectedKey()).toBeUndefined();
    expect(t.store.adopted()).toBeUndefined();
    t.dispose();
  });

  it("another session's selection ends the adoption, Return to filters ends it and the reveal", async () => {
    const t = setup([card("a", { tag: "s4" }), card("b")]);
    await t.started;
    const s4 = t.list(4);
    t.store.selectCreation("a");
    t.emit(card("a", { tag: "s4", state: "bound", show_pending: false, bound_session: "claude/sample-project/s4" }));
    expect(t.store.adopted()).toBe(s4.key);
    // A card's selection keeps it (selectSession returns before 3442).
    t.store.selectCreation("b");
    expect(t.store.adopted()).toBe(s4.key);
    t.setSelectedKey("\nrun:99");
    expect(t.store.adopted()).toBeUndefined();
    t.store.choose("b");
    expect(t.store.revealed()).toBe("b");
    t.store.returnToFilters();
    expect(t.store.revealed()).toBeUndefined();
    t.dispose();
  });

  it("a search change ends the reveal", async () => {
    const t = setup([card("a")]);
    await t.started;
    t.store.choose("a");
    t.setScope({ filter: "all", hosts: [], query: "x", projects: [] });
    expect(t.store.revealed()).toBeUndefined();
    t.dispose();
  });
});

describe("the status menu (463-480)", () => {
  it("reveals, selects and shows the chosen card on the Sessions list", async () => {
    const t = setup([card("a", { show_pending: false })]);
    await t.started;
    const entry = t.store.creationStatusMenu()[0] as MenuEntryLike;
    expect(entry.label).toBe("tag-a: Queued");
    entry.action!();
    expect(t.store.revealed()).toBe("a");
    expect(t.store.selectedCreation()).toBe("a");
    expect(t.page()).toBe("sessions");
    expect(t.revealed).toEqual([creationRowId("a")]);
    expect(t.calls.filter((c) => c.name === "creationAction").map((c) => c.args)).toEqual([["a", "show"]]);
    t.dispose();
  });

  it("says a resolved creation is resolved", async () => {
    const t = setup([card("a")]);
    await t.started;
    const entry = t.store.creationStatusMenu()[0] as MenuEntryLike;
    t.emit({ id: "a", removed: true });
    entry.action!();
    expect(t.notices).toEqual([{ tone: "info", text: RESOLVED, sticky: undefined }]);
    expect(t.store.selectedCreation()).toBeUndefined();
    t.dispose();
  });
});

describe("the admission (2911-2919)", () => {
  it("selects and reveals the new card, expands its project, shows Sessions and hides an editable predecessor", async () => {
    const t = setup([card("old", { state: "failed", can_edit: true })], { grouping: grouping([header("alpha", true), header("ungrouped")]) });
    await t.started;
    t.store.admitted("new", { group: "alpha", predecessor: "old" });
    expect(t.calls.filter((c) => c.name === "creationAction").map((c) => c.args)).toEqual([["old", "hide"]]);
    expect(t.store.selectedCreation()).toBe("new");
    expect(t.store.revealed()).toBe("new");
    expect(t.collapsed).toEqual([["alpha", false]]);
    expect(t.page()).toBe("sessions");
    expect(t.revealed).toEqual([creationRowId("new")]);
    // The card arrives after the admission.
    t.emit(card("new"));
    expect(t.store.selectedCard()?.id).toBe("new");
    t.dispose();
  });

  it("keeps a predecessor that can no longer be edited", async () => {
    const t = setup([card("old", { state: "launching" })]);
    await t.started;
    t.store.admitted("new", { group: "ungrouped", predecessor: "old" });
    expect(t.calls.filter((c) => c.name === "creationAction")).toEqual([]);
    expect(t.collapsed).toEqual([]);
    t.dispose();
  });
});

describe("Edit choices… (1525-1528)", () => {
  const plan = { id: "a", cmd: "codex", group: "alpha", host: "studio", account: "work", tag: "retry" };

  it("opens New session with the operation as its recipe", async () => {
    const t = setup([card("a", { state: "failed", can_edit: true })], { plans: { a: plan } });
    await t.started;
    await t.store.editChoices("a");
    expect(t.opened).toEqual([{ agent: "codex", project: "alpha", computer: "studio", account: "work", recipe: plan }]);
    t.dispose();
  });

  it("does nothing once the creation cannot be edited", async () => {
    const t = setup([card("a", { state: "failed", can_edit: true }), card("b", { state: "failed" })], { plans: { a: null } });
    await t.started;
    await t.store.editChoices("a");
    await t.store.editChoices("b");
    expect(t.opened).toEqual([]);
    expect(t.calls.filter((c) => c.name === "creationPlan").map((c) => c.args)).toEqual([["a"]]);
    t.dispose();
  });
});

describe("the notices (1577-1586)", () => {
  // A creation made after this window started (unix seconds); older failures stay quiet.
  const LIVE = 4_000_000_000;
  it.each(["failed", "unknown", "storage_error"].flatMap((state) => [-1, 0, 1].map((offset) => ({ state, offset }))))(
    "uses last state change for a first-seen old creation ($state, $offset seconds from start)", async ({ state, offset }) => {
      vi.useFakeTimers();
      const now = 1_794_000_000;
      vi.setSystemTime(now * 1000);
      const failed = card("late", { state, error: "Late failure.", created: now - 3600, updated: now + offset });
      const t = setup([failed]);
      try {
        await t.started;
        expect(t.store.creations()[0]).toMatchObject({ error: "Late failure.", created: now - 3600, updated: now + offset });
        expect(t.notices).toHaveLength(offset < 0 ? 0 : 1);
        t.emit(failed);
        expect(t.notices).toHaveLength(offset < 0 ? 0 : 1);
      } finally { t.dispose(); vi.useRealTimers(); }
    },
  );
  it("show once per state and error, using the normal toast timer", async () => {
    const t = setup([card("a", { state: "failed", caption: "Creation failed", error: "No folder.", created: LIVE })]);
    await t.started;
    t.emit(card("a", { state: "failed", caption: "Creation failed", error: "No folder.", created: LIVE }));
    t.emit(card("a", { state: "failed", caption: "Creation failed", error: "Still no folder.", created: LIVE }));
    t.emit(card("a", { state: "queued", created: LIVE }));
    t.emit(card("a", { state: "unknown", caption: "Creation result unknown", error: "Still no folder.", created: LIVE }));
    expect(t.notices).toEqual([
      { tone: "error", text: "Creation tag-a: Creation failed\nNo folder.", sticky: undefined },
      { tone: "error", text: "Creation tag-a: Creation failed\nStill no folder.", sticky: undefined },
      { tone: "error", text: "Creation tag-a: Creation result unknown\nStill no folder.", sticky: undefined },
    ]);
    t.dispose();
  });

  it("expires failed and unknown toasts on the error timer while retaining card details and once-per-state delivery", async () => {
    vi.useFakeTimers();
    const bus = createNoticeBus();
    const failed = card("a", { state: "failed", error: "Name already exists.", can_edit: true, created: LIVE });
    const t = setup([failed], { bus });
    try {
      await t.started;
      expect(bus.notices()).toHaveLength(1); expect(bus.notices()[0].sticky).toBe(false);
      vi.advanceTimersByTime(ERROR_NOTICE_MS); expect(bus.notices()).toHaveLength(0);
      expect(t.store.creations()[0].error).toBe("Name already exists.");
      t.emit(failed); expect(bus.notices()).toHaveLength(0);
      const unknown = { ...failed, state: "unknown", can_edit: false };
      t.emit(unknown); expect(bus.notices()).toHaveLength(1);
      vi.advanceTimersByTime(ERROR_NOTICE_MS); expect(bus.notices()).toHaveLength(0);
      t.emit(unknown); expect(bus.notices()).toHaveLength(0);
      expect(t.store.creations()[0].state).toBe("unknown");
    } finally { t.dispose(); vi.useRealTimers(); }
  });

  it("keep a failure that settled before the window started on its card, without a toast at every restart", async () => {
    const old = 1_793_000_000;
    const t = setup([
      card("gone", { state: "failed", caption: "Creation failed", error: "This session already has an account.", created: old }),
      card("busy", { state: "queued", created: old }),
    ]);
    await t.started;
    // The settled failure is still on its card, but raises no toast.
    expect(t.store.creations().find((c) => c.id === "gone")?.error).toBe("This session already has an account.");
    t.emit(card("gone", { state: "failed", caption: "Creation failed", error: "This session already has an account.", created: old }));
    expect(t.notices).toEqual([]);
    // A creation seen in progress that fails while the window is open shows one.
    t.emit(card("busy", { state: "failed", caption: "Creation failed", error: "No folder.", created: old }));
    // So does a new failure of the settled card, and a creation first seen failing after the start.
    t.emit(card("gone", { state: "unknown", caption: "Creation result unknown", error: "Lost.", created: old }));
    t.emit(card("fresh", { state: "failed", caption: "Creation failed", error: "Name already exists.", created: LIVE }));
    expect(t.notices.map((n) => n.text)).toEqual([
      "Creation tag-busy: Creation failed\nNo folder.",
      "Creation tag-gone: Creation result unknown\nLost.",
      "Creation tag-fresh: Creation failed\nName already exists.",
    ]);
    t.dispose();
  });

  it("remember 128 creations", async () => {
    const t = setup();
    await t.started;
    for (let i = 0; i < 129; i++) t.emit(card(`n${i}`, { state: "storage_error", created: LIVE + i }));
    expect(t.notices).toHaveLength(129);
    // The newest is remembered; the oldest was forgotten and shows again.
    t.emit(card("n128", { state: "storage_error", created: LIVE + 128 }));
    expect(t.notices).toHaveLength(129);
    t.emit(card("n0", { state: "storage_error", created: LIVE }));
    expect(t.notices).toHaveLength(130);
    t.dispose();
  });
});

describe("the footer and the search", () => {
  it("counts the shown cards, also in a collapsed project, never in Archive", async () => {
    const t = setup([card("a"), card("b", { show_pending: false }), card("c", { group: "ungrouped" })], { grouping: grouping([header("alpha", true), header("ungrouped")]) });
    await t.started;
    expect(t.store.creationFooter(true)).toBe(", 2 creations");
    expect(t.store.creationFooter(false)).toBe("2 creations");
    t.setScope({ filter: "archived", hosts: [], query: "", projects: [] });
    expect(t.store.creationFooter(true)).toBe("");
    t.dispose();
  });

  it("lists the cards a search shows, for the search results (SessionSearch.cpp:287-305, zerus-5wmh.13.17.3)", async () => {
    const t = setup([card("a"), card("b"), card("c", { show_pending: false })], { details: { a: "tag-a\n\nFolder: /workspace/match", b: "tag-b", c: "tag-c match" } });
    await t.started;
    expect(t.store.searchCards()).toEqual([]);
    t.setScope({ filter: "all", hosts: [], query: "MATCH", projects: [] });
    await flush();
    expect(t.store.searchCards().map((c) => c.id)).toEqual(["a"]);
    t.setScope({ filter: "active", hosts: [], query: "match", projects: [] });
    await flush();
    expect(t.store.searchCards().map((c) => c.id)).toEqual(["a"]);
    for (const filter of ["archived", "paused", "attention"] as const) {
      t.setScope({ filter, hosts: [], query: "match", projects: [] });
      await flush();
      expect(t.store.searchCards()).toEqual([]);
    }
    t.dispose();
  });

  it("reads each card's details once per change while searching", async () => {
    const t = setup([card("a"), card("b")], { details: { a: "tag-a\n\nFolder: /workspace/match", b: "tag-b" } });
    await t.started;
    expect(t.calls.filter((c) => c.name === "creationDetails")).toEqual([]);
    t.setScope({ filter: "all", hosts: [], query: "match", projects: [] });
    await flush();
    expect(t.calls.filter((c) => c.name === "creationDetails").map((c) => c.args[0]).sort()).toEqual(["a", "b"]);
    expect(t.store.creationFooter(false)).toBe("1 creation");
    t.setScope({ filter: "all", hosts: [], query: "matc", projects: [] });
    await flush();
    expect(t.calls.filter((c) => c.name === "creationDetails")).toHaveLength(2);
    t.emit(card("b", { state: "checking" }));
    await flush();
    expect(t.calls.filter((c) => c.name === "creationDetails")).toHaveLength(3);
    t.dispose();
  });
});

describe("a card never opens a composer or touches drafts", () => {
  it("sends only the creation commands through every card action", async () => {
    const t = setup([card("a", { state: "failed", can_edit: true, can_resume: true }), card("b", { state: "unknown" })], { plans: { a: { id: "a", cmd: "claude" } } });
    await t.started;
    t.store.selectCreation("a");
    for (const action of ["resume", "check", "stop", "hide", "show"] as const) await t.store.act("a", action);
    await t.store.editChoices("a");
    (t.store.creationStatusMenu()[1] as MenuEntryLike).action!();
    t.store.returnToFilters();
    t.store.admitted("c", { group: "alpha", predecessor: "a" });
    t.setScope({ filter: "all", hosts: [], query: "tag", projects: [] });
    await flush();
    const names = new Set(t.calls.map((c) => c.name));
    expect([...names].filter((n) => !CREATION_COMMANDS.includes(n))).toEqual([]);
    // Each action named its exact operation.
    expect(t.calls.filter((c) => c.name === "creationAction").map((c) => c.args)).toEqual([
      ["a", "resume"], ["a", "check"], ["a", "stop"], ["a", "hide"], ["a", "show"], ["b", "show"], ["a", "hide"],
    ]);
    // A selected card leaves no session selected.
    expect(t.selectedKey()?.startsWith("#launch\n")).toBe(true);
    t.dispose();
  });

  it("shows a lost action, and sends it once", async () => {
    const t = setup([card("a")], { refuse: "This creation is no longer pending." });
    await t.started;
    await t.store.act("a", "check");
    expect(t.notices).toEqual([{ tone: "error", text: "This creation is no longer pending.", sticky: true }]);
    expect(t.calls.filter((c) => c.name === "creationAction")).toHaveLength(1);
    t.dispose();
  });

  it("stays silent when the host says the creation is already resolved", async () => {
    const t = setup([card("a")], { refuse: RESOLVED });
    await t.started;
    await t.store.act("a", "check");
    expect(t.notices).toEqual([]);
    t.dispose();
  });

  it("stays silent when the card resolves before its action's answer crosses back", async () => {
    const t = setup([card("a")], { refuse: "The answer was lost." });
    await t.started;
    const action = t.store.act("a", "check");
    t.emit({ id: "a", removed: true });
    await action;
    expect(t.notices).toEqual([]);
    t.dispose();
  });
});
