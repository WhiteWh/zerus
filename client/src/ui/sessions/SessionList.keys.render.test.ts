// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import type { Row } from "./model";

// The session list's keys, after Qt's SessionList::keyPressEvent
// (SessionList.cpp:585-632 at f5a822649^) and its Enter (SessionsWindow.cpp:565,5499-5504).

const fixture = vi.hoisted(() => ({
  rows: () => [] as Row[], index: 0,
  collapse: vi.fn((_project: string, _collapsed: boolean) => Promise.resolve()),
}));
vi.mock("../../bridge", async (original) => ({ ...await original<object>(), gitRows: vi.fn().mockResolvedValue(undefined) }));
vi.mock("./model", async (original) => ({ ...await original<object>(), listRows: () => fixture.rows() }));
vi.mock("../../store/organization", async (original) => ({
  ...await original<object>(), groupOf: (key: string) => (key.includes(":a") ? "alpha" : "beta"), setProjectCollapsed: fixture.collapse,
}));
vi.mock("@tanstack/solid-virtual", () => ({
  createVirtualizer: (options: { count: number; getItemKey(n: number): string }) => ({
    measure() {}, _willUpdate() {}, scrollToIndex() {}, getTotalSize: () => options.count * 94,
    getVirtualItems: () => options.count ? [{ key: options.getItemKey(fixture.index), start: fixture.index * 94, index: fixture.index }] : [],
  }),
}));
import { SessionList } from "./SessionList";
import { applyBatch, catalog } from "../../store/catalog";
import { emptyBatch } from "../../types";
import { detailTab, latestRequests, multi, selectedKey, setDetailTab, setMulti, setPage, setSelectedKey, setStripMode } from "../../store/ui";
import { clearMulti, setAnchor } from "./organize";

const KEYS = ["a0", "a1", "a2", "b0"];
const key = (name: string) => `\nrun:${name}`;
const header = (project: string, collapsed = false): Row => ({ type: "header", id: `h:${project}`, project, name: project, color: null, palette: 0, collapsed, total: 0, attention: 0, unread: 0, working: 0 });
const card = (name: string): Row => ({ type: "card", id: key(name), session: catalog.rows[key(name)], expandable: false, expanded: false, compactRow: false });
const open = (collapsedAlpha = false): Row[] => collapsedAlpha
  ? [header("alpha", true), header("beta"), card("b0")]
  : [header("alpha"), card("a0"), card("a1"), card("a2"), header("beta"), card("b0")];

let dispose: (() => void) | undefined;
const settle = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
let list!: HTMLElement;
const press = (k: string, mods: Partial<KeyboardEventInit> = {}, target: Element = list) =>
  target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...mods }));
const selected = () => KEYS.filter((name) => multi[key(name)]);

beforeEach(async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  setPage("sessions"); setStripMode(false);
  applyBatch({ ...emptyBatch(), replaces: [{ computer: "", rows: KEYS.map((name) => ({
    computer: "", key: `run:${name}`, row: { name: `codex/fixture/${name}`, agent: "codex", status: "ready", state: "running", cwd: `/fixture/${name}`, run_id: name },
  })) }] });
  // The rendered row is the alpha project's header: its button is focusable.
  fixture.rows = () => open(); fixture.index = 0;
  for (const name of KEYS) setMulti(key(name), false);
  setSelectedKey(key("a0")); setAnchor(key("a0")); setDetailTab("terminal");
  const host = document.createElement("div"); document.body.append(host);
  dispose = render(() => createComponent(SessionList, {}), host);
  await settle();
  list = host.querySelector(".sp-scroll")!;
});
afterEach(async () => {
  dispose?.(); dispose = undefined; await settle();
  document.body.replaceChildren(); vi.clearAllMocks(); vi.unstubAllGlobals();
});

describe("the session list's keys", () => {
  it("Shift+Down, Up, End and Home extend the range from the open conversation, which stays open", () => {
    press("ArrowDown", { shiftKey: true });
    expect(selected()).toEqual(["a0", "a1"]);
    press("ArrowDown", { shiftKey: true });
    expect(selected()).toEqual(["a0", "a1", "a2"]);
    press("ArrowUp", { shiftKey: true });
    expect(selected()).toEqual(["a0", "a1"]);
    press("End", { shiftKey: true });
    expect(selected()).toEqual(["a0", "a1", "a2", "b0"]);
    press("Home", { shiftKey: true });
    expect(selected()).toEqual(["a0"]);
    expect(selectedKey()).toBe(key("a0"));
  });

  it("Ctrl+Shift adds the range to the selection, and a plain move starts the next range there", () => {
    setMulti(key("b0"), true);
    press("ArrowDown", { shiftKey: true, ctrlKey: true });
    expect(selected()).toEqual(["a0", "a1", "b0"]);
    press("ArrowDown");
    expect(selectedKey()).toBe(key("a1"));
    press("ArrowDown", { shiftKey: true });
    expect(selected()).toEqual(["a1", "a2"]);
  });

  it("Escape or Deselect all ends the range too: the next Shift key starts again from the open conversation (zerus-m0dem)", () => {
    press("ArrowDown", { shiftKey: true });
    press("ArrowDown", { shiftKey: true });
    expect(selected()).toEqual(["a0", "a1", "a2"]);
    press("Escape");
    expect(selected()).toEqual([]);
    press("ArrowDown", { shiftKey: true });
    expect(selected(), "after Escape").toEqual(["a0", "a1"]);
    press("ArrowDown", { shiftKey: true });
    clearMulti();
    expect(selected()).toEqual([]);
    press("ArrowDown", { shiftKey: true });
    expect(selected(), "after Deselect all").toEqual(["a0", "a1"]);
  });

  it("arrows, Home and End step over an ended parent's container: it is never a selection (zerus-sa9qn.17)", () => {
    const box: Row = { type: "container", id: "container:ended:\nm\ns9", project: "alpha", depth: 0,
      container: { name: "codex/fixture/s9", title: "s9", agent: "codex", computer: "", machine: "workstation", state: "archived" } };
    fixture.rows = () => [header("alpha"), box, card("a0"), card("a1"), header("beta"), box, card("b0")];
    setSelectedKey(key("a1"));
    press("ArrowUp"); expect(selectedKey()).toBe(key("a0"));
    press("ArrowUp"); expect(selectedKey()).toBe(key("a0"));
    press("Home"); expect(selectedKey()).toBe(key("a0"));
    press("End"); expect(selectedKey()).toBe(key("b0"));
    press("ArrowUp"); expect(selectedKey()).toBe(key("a1"));
  });

  it("Home and End go to the first and last row", () => {
    press("End");
    expect(selectedKey()).toBe(key("b0"));
    press("Home");
    expect(selectedKey()).toBe(key("a0"));
  });

  it("Left collapses a plain row's project and Right expands it again; arrows go on past it", () => {
    setSelectedKey(key("a1"));
    press("ArrowRight");
    expect(fixture.collapse).not.toHaveBeenCalled();
    press("ArrowLeft");
    expect(fixture.collapse).toHaveBeenCalledExactlyOnceWith("alpha", true);
    fixture.rows = () => open(true);
    // The open conversation's row is hidden now: Down goes to the next project, Right reopens it.
    press("ArrowRight");
    expect(fixture.collapse).toHaveBeenLastCalledWith("alpha", false);
    press("ArrowDown");
    expect(selectedKey()).toBe(key("b0"));
  });

  it("Enter on the list shows the latest Activity; a focused project header keeps its own Enter", () => {
    const before = latestRequests();
    press("Enter");
    expect(detailTab()).toBe("activity");
    expect(latestRequests()).toBe(before + 1);
    setDetailTab("terminal");
    const button = list.querySelector("button");
    expect(button).not.toBeNull();
    press("Enter", {}, button!);
    expect(latestRequests()).toBe(before + 1);
    expect(detailTab()).toBe("terminal");
  });
});
