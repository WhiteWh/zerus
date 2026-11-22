// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
vi.mock("@tanstack/solid-virtual", () => ({ createVirtualizer: (opts: { count: number; getItemKey(n: number): string }) => ({
  measure() {}, _willUpdate() {}, scrollToIndex() {}, getTotalSize: () => opts.count * 94,
  getVirtualItems: () => Array.from({ length: opts.count }, (_, index) => ({ key: opts.getItemKey(index), start: index * 94, index })),
}) }));
import { SessionList, sessionMenu } from "./SessionList";
import { applyBatch, catalog } from "../../store/catalog";
import { applyMachines } from "../../store/machines";
import { applyGrouping } from "../../store/organization";
import { emptyBatch, type ViewRow } from "../../types";
import { emptyGrouping } from "../../store/organization";
import { listRows, filterCounts } from "./model";
import { favorite, setFavorite } from "../../store/favorites";
import { setTeamCollapsed, teamCollapsed } from "../../store/teamCollapse";
import { menu, setMenu, setStateFilter, setStripMode, setMachineFilter, setProjectFilter, setAccountFilter, setFolderFilter, setCompactRows } from "../../store/ui";

const MACHINE = "11111111-1111-4111-8111-111111111111";
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const key = (n: number) => `\nrun:favorite-${n}`;
let stop: (() => void) | undefined;
const raw = (n: number, extra: Partial<ViewRow> = {}): ViewRow => ({ name: `codex/example/favorite-${n}`, title: `Example ${n}`,
  agent: "codex", session_id: id(n), run_id: `favorite-${n}`, conversation_id: `conversation-${n}`, state: "running", status: "ready", ...extra });
function grouping(collapsed: boolean) {
  applyGrouping({ ...emptyBatch(), grouping: { ...emptyGrouping(), loaded: true, ready: true, default_project: "project", headers: [
    { id: "project", name: "Project", color: null, palette: 0, collapsed, accessible: true, is_default: true },
    { id: "other", name: "Other", color: null, palette: 1, collapsed: false, accessible: true, is_default: false },
  ] } });
}
function setup() {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  applyMachines({ ...emptyBatch(), machines: [{ computer: "", machine: { machine_id: MACHINE } as never }] });
  applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }], replaces: [{ computer: "", rows: [
    { computer: "", key: "run:favorite-1", row: raw(1, { state: "paused", status: "paused" }) },
    ...[2, 3].map((n) => ({ computer: "", key: `run:favorite-${n}`, row: raw(n, { parent: {
      machine: MACHINE, session_id: id(1), conversation_id: "conversation-1", run_id: "favorite-1", name: raw(1).name, state: "saved" as const,
    } }) })),
  ] }] });
  grouping(false); setStateFilter("all"); setStripMode(false);
  for (const s of Object.values(catalog.rows)) setFavorite(s, false);
  for (const n of [1, 2, 3, 4]) setTeamCollapsed(key(n), false);
  const host = document.createElement("div"); document.body.append(host);
  stop = render(() => createComponent(SessionList, {}), host);
  return host;
}
afterEach(() => { stop?.(); document.body.replaceChildren(); setMenu(undefined); setMachineFilter([]); setProjectFilter([]); setAccountFilter([]); setFolderFilter(undefined); vi.unstubAllGlobals(); });
const cards = () => listRows().flatMap((r) => r.type === "card" ? [r.id] : []);

describe("actual session-row favorites menu and model", () => {
  it.each([false, true])("opens a retained nonfavorite parent under a collapsed ancestor (compact: %s)", (compact) => {
    const host = setup();
    const a = compact ? 20 : 10, b = a + 1, f = a + 2, sibling = a + 3, unrelated = a + 4;
    const parent = (n: number) => ({ machine: MACHINE, session_id: id(n), conversation_id: `conversation-${n}`, run_id: `favorite-${n}`, name: raw(n).name, state: "live" as const });
    applyBatch({ ...emptyBatch(), replaces: [{ computer: "", rows: [
      { computer: "", key: key(a).slice(1), row: raw(a) },
      { computer: "", key: key(b).slice(1), row: raw(b, { parent: parent(a), subagents: { native: { state: "working", name: "Native child" } } }) },
      { computer: "", key: key(f).slice(1), row: raw(f, { parent: parent(b) }) },
      { computer: "", key: key(sibling).slice(1), row: raw(sibling, { parent: parent(b) }) },
      { computer: "", key: key(unrelated).slice(1), row: raw(unrelated, { parent: parent(a) }) },
    ] }] });
    setFavorite(catalog.rows[key(f)], true); setTeamCollapsed(key(a), true); setCompactRows(key(b), compact);
    const toggle = () => host.querySelector<HTMLButtonElement>(`.vrow[data-key='${CSS.escape(key(b))}'] ${compact ? ".cc-team" : ".sc-team"}`)!;
    const native = () => listRows().some((r) => r.type === "child" && r.parent.key === key(b));
    expect(cards()).toEqual([key(a), key(b), key(f)]);
    const beforeExpanded = toggle().getAttribute("aria-expanded");
    toggle().click();
    expect({ beforeExpanded, afterExpanded: toggle().getAttribute("aria-expanded"), cards: cards(), native: native() }).toEqual({
      beforeExpanded: "false", afterExpanded: "true", cards: [key(a), key(b), key(f), key(sibling)], native: true,
    });
    expect(teamCollapsed(key(a))).toBe(true);
    toggle().click();
    expect(cards()).toEqual([key(a), key(b), key(f)]); expect(native()).toBe(false);
    setTeamCollapsed(key(a), false);
    expect(cards()).toEqual([key(a), key(b), key(f), key(unrelated)]);
    toggle().click(); setTeamCollapsed(key(a), true);
    expect(cards()).toEqual([key(a), key(b), key(f), key(sibling)]); expect(native()).toBe(true);
    grouping(true);
    expect(cards()).toEqual([key(a), key(b), key(f), key(sibling)]);
    setFavorite(catalog.rows[key(f)], false);
    expect(cards()).toEqual([]);
  });
  it("toggles a favorite's own children while its ancestor stays collapsed", () => {
    const host = setup();
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:favorite-4", row: raw(4, { parent: {
      machine: MACHINE, session_id: id(2), conversation_id: "conversation-2", run_id: "favorite-2", name: raw(2).name, state: "live",
    } }) }] });
    setFavorite(catalog.rows[key(2)], true); setTeamCollapsed(key(1), true); setTeamCollapsed(key(2), true);
    const toggle = () => host.querySelector<HTMLButtonElement>(`.vrow[data-key='${CSS.escape(key(2))}'] button[title$='subagents and child sessions']`)!;
    expect(cards()).toEqual([key(1), key(2)]);
    expect(toggle()).not.toBeNull(); toggle().click();
    expect(teamCollapsed(key(1))).toBe(true);
    expect(teamCollapsed(key(2))).toBe(false);
    expect(cards()).toEqual([key(1), key(2), key(4)]);
    expect(host.querySelector(`.vrow[data-key='${CSS.escape(key(4))}']`)).not.toBeNull();
    toggle().click();
    expect(cards()).toEqual([key(1), key(2)]);
    setTeamCollapsed(key(1), false);
    expect(cards()).toEqual([key(1), key(2), key(3)]);
    expect(teamCollapsed(key(2))).toBe(true);
  });
  it("adds from a real child row menu, keeps it in collapsed groups, then removes it there", () => {
    const host = setup(); const before = { ...filterCounts() };
    const child = host.querySelector<HTMLElement>(`.vrow[data-key='${CSS.escape(key(2))}'] .rowhit`)!;
    expect(child).not.toBeNull(); child.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const add = menu()?.items.find((v) => "label" in v && v.label === "Add to favorites");
    expect(add).toMatchObject({ disabled: false }); if (add && "action" in add) add.action?.();
    expect(favorite(catalog.rows[key(2)])).toBe(true);
    expect(host.querySelectorAll(".sc-favorite")).toHaveLength(1);
    setTeamCollapsed(key(1), true); grouping(true);
    expect(cards()).toEqual([key(1), key(2)]); expect(filterCounts()).toEqual(before);
    const retained = host.querySelector<HTMLElement>(`.vrow[data-key='${CSS.escape(key(2))}'] .rowhit`)!;
    retained.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const remove = menu()?.items.find((v) => "label" in v && v.label === "Remove from favorites");
    expect(remove).toBeDefined(); if (remove && "action" in remove) remove.action?.();
    expect(cards()).toEqual([]); expect(filterCounts()).toEqual(before);
  });
  it("survives a rename and refreshed row key, but never a reused name/key with a new identity", () => {
    setup(); setFavorite(catalog.rows[key(2)], true);
    const pending = sessionMenu(catalog.rows[key(2)]).find((v) => "label" in v && v.label === "Remove from favorites");
    applyBatch({ ...emptyBatch(), removes: [{ computer: "", key: "run:favorite-2" }], upserts: [{ computer: "", key: "run:renamed", row: raw(2, { name: "codex/new/title", title: "Renamed" }) }] });
    expect(favorite(catalog.rows["\nrun:renamed"])).toBe(true);
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:favorite-2", row: raw(2, { session_id: id(20) }) }] });
    expect(favorite(catalog.rows[key(2)])).toBe(false);
    if (pending && "action" in pending) pending.action?.();
    expect(favorite(catalog.rows["\nrun:renamed"])).toBe(true);
  });
  it("never bypasses state, machine, project, account or folder filters", () => {
    setup(); setFavorite(catalog.rows[key(2)], true); setTeamCollapsed(key(1), true); grouping(true);
    expect(cards()).toContain(key(2)); setStateFilter("paused"); expect(cards()).not.toContain(key(2)); setStateFilter("all");
    setMachineFilter(["absent"]); expect(cards()).not.toContain(key(2)); setMachineFilter([]);
    setProjectFilter(["other"]); expect(cards()).not.toContain(key(2)); setProjectFilter([]);
    setAccountFilter(["absent"]); expect(cards()).not.toContain(key(2)); setAccountFilter([]);
    setFolderFilter({ computer: "", path: "/missing", style: "posix", checkout: false }); expect(cards()).not.toContain(key(2));
  });
  it("does not offer a usable preference for a row without durable identity", () => {
    setup(); const unbound = { ...catalog.rows[key(2)], sessionId: undefined };
    expect(sessionMenu(unbound).find((v) => "label" in v && v.label === "Add to favorites")).toMatchObject({ disabled: true });
  });
  it("refuses an Add menu snapshot after its row key was reused for another session", () => {
    setup();
    const add = sessionMenu(catalog.rows[key(2)]).find((v) => "label" in v && v.label === "Add to favorites");
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:favorite-2", row: raw(2, { session_id: id(20) }) }] });
    if (add && "action" in add) add.action?.();
    expect(favorite(catalog.rows[key(2)])).toBe(false);
  });
  it("distinguishes identical names/session UUIDs on two computers and follows a canonical computer alias", () => {
    setup(); setFavorite(catalog.rows[key(2)], true);
    applyMachines({ ...emptyBatch(), machines: [{ computer: "peer", machine: { machine_id: id(90) } as never }] });
    applyBatch({ ...emptyBatch(), computers: [{ computer: "peer", status: "live", attempt: 0 }], upserts: [{ computer: "peer", key: "run:favorite-2", row: raw(2) }] });
    expect(favorite(catalog.rows["peer\nrun:favorite-2"])).toBe(false);
    applyMachines({ ...emptyBatch(), machines: [{ computer: "new-alias", machine: { machine_id: MACHINE } as never }] });
    applyBatch({ ...emptyBatch(), computers: [{ computer: "new-alias", status: "live", attempt: 0 }], upserts: [{ computer: "new-alias", key: "run:alias", row: raw(2, { name: "codex/renamed/alias" }) }] });
    expect(favorite(catalog.rows["new-alias\nrun:alias"])).toBe(true);
  });
});
