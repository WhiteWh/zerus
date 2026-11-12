// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";

const fixture = vi.hoisted(() => ({
  canAttach: vi.fn(() => true), image: vi.fn(async (_file: Blob) => {}), send: vi.fn(),
  paste: vi.fn(), input: vi.fn(), show: vi.fn(), hide: vi.fn(), capture: vi.fn(), terminalImage: vi.fn(async (_png: Uint8Array) => {}), imageCurrent: true,
  copy: vi.fn(async (_text: string) => {}), clipboard: vi.fn(async () => "pasted from the menu"),
  selection: "", mouse: "none", keys: undefined as ((event: KeyboardEvent) => boolean) | undefined,
  generation: (): number => 7,
}));
vi.mock("../../bridge", async (importOriginal) => ({ ...(await importOriginal<typeof import("../../bridge")>()), copyVerbatim: fixture.copy, pasteText: fixture.clipboard }));
vi.mock("../../store/composer", () => ({ composer: { canAttach: fixture.canAttach, pasteImage: fixture.image, send: fixture.send, view: () => undefined } }));
vi.mock("../../store/terminal", () => ({
  screenShowsLatest: () => true,
  terminal: { paste: fixture.paste, input: fixture.input, show: fixture.show, hide: fixture.hide, generation: () => fixture.generation(),
    captureImage: (screen: unknown) => { fixture.capture(screen); return { current: () => fixture.imageCurrent, paste: async (png: Uint8Array) => { if (fixture.imageCurrent) await fixture.terminalImage(png); } }; },
    connected: () => false, ready: () => false, texts: () => undefined, status: () => "Fixture terminal", resized() {} },
}));
// Only xterm's canvas/OS renderer is inert. The actual tab and capture listener run.
vi.mock("@xterm/xterm", () => ({ Terminal: class {
  cols = 80; rows = 24; options: Record<string, unknown>; textarea?: HTMLTextAreaElement;
  constructor(options: Record<string, unknown>) { this.options = options; }
  data?: (text: string) => void;
  loadAddon() {} onData(fn: (text: string) => void) { this.data = fn; } onBinary() {} onResize() {} write(_bytes: unknown, done?: () => void) { done?.(); }
  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean) { fixture.keys = handler; }
  getSelection() { return fixture.selection; } hasSelection() { return !!fixture.selection; }
  get modes() { return { mouseTrackingMode: fixture.mouse }; }
  open(host: HTMLElement) { this.textarea = document.createElement("textarea"); host.append(this.textarea); }
  focus() { this.textarea?.focus(); } dispose() { this.textarea?.remove(); } reset() {} paste(text: string) { this.data?.(text); }
} }));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit() {} } }));

import { TerminalTab } from "./Terminal";
import { detailTab, menu, setDetailTab, setMenu } from "../../store/ui";

let dispose: (() => void) | undefined, host: HTMLDivElement;
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
beforeEach(async () => {
  vi.clearAllMocks(); fixture.canAttach.mockReturnValue(true); fixture.imageCurrent = true;
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  setDetailTab("terminal");
  host = document.createElement("div"); document.body.append(host);
  dispose = render(() => createComponent(TerminalTab, {}), host);
  await settle();
});
afterEach(() => {
  dispose?.(); dispose = undefined; document.body.replaceChildren(); vi.unstubAllGlobals(); setMenu(undefined);
  fixture.selection = ""; fixture.mouse = "none"; fixture.generation = () => 7;
  fixture.paste.mockReset(); fixture.input.mockReset(); fixture.clipboard.mockReset(); fixture.clipboard.mockImplementation(async () => "pasted from the menu");
});

function paste(data: Partial<DataTransfer>, target = host.querySelector(".term-host")!) {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", { value: { items: [], files: [], types: [], getData: () => "", ...data } });
  target.dispatchEvent(event);
  return event;
}
const png = () => Uint8Array.from([137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3]);
const image = () => new File([png()], "clipboard.png", { type: "image/png" });

it("keeps the exact event PNG in Terminal without touching the draft or sending Enter", async () => {
  await mounted();
  const file = image(), getAsFile = vi.fn(() => file), bubbled = vi.fn();
  document.addEventListener("paste", bubbled, { once: true });
  try {
    const event = paste({ items: [{ kind: "file", type: file.type, getAsFile }] as unknown as DataTransferItemList,
      getData: () => "text alongside the screenshot must not be typed" });
    await settle();
    expect(event.defaultPrevented).toBe(true);
    expect(getAsFile).toHaveBeenCalledTimes(1);
    expect(detailTab()).toBe("terminal");
    expect(fixture.capture).toHaveBeenCalledExactlyOnceWith(fixture.show.mock.calls.at(-1)![0]);
    expect(fixture.terminalImage).toHaveBeenCalledExactlyOnceWith(png());
    expect(fixture.image).not.toHaveBeenCalled(); expect(fixture.paste).not.toHaveBeenCalled();
    expect(fixture.input).not.toHaveBeenCalled(); expect(fixture.send).not.toHaveBeenCalled(); expect(bubbled).not.toHaveBeenCalled();
  } finally { document.removeEventListener("paste", bubbled); }
});

it("uses one image from files-only or mixed image clipboard data independently of composer readiness", async () => {
  await mounted(); fixture.canAttach.mockReturnValue(false);
  paste({ files: [image(), image()] as unknown as FileList, getData: () => "never this text" });
  await settle();
  expect(fixture.terminalImage).toHaveBeenCalledExactlyOnceWith(png());
  expect(fixture.image).not.toHaveBeenCalled(); expect(fixture.paste).not.toHaveBeenCalled(); expect(detailTab()).toBe("terminal");
});

it("refuses unreadable advertised image data without text or an Activity fallback", async () => {
  await mounted();
  paste({ items: [{ kind: "file", type: "image/png", getAsFile: () => null }] as unknown as DataTransferItemList, getData: () => "not image bytes" });
  expect(host.querySelector('[role="status"]')).not.toBeNull();
  expect(fixture.terminalImage).not.toHaveBeenCalled(); expect(fixture.image).not.toHaveBeenCalled(); expect(fixture.paste).not.toHaveBeenCalled();
  expect(host.textContent).not.toContain("Open Activity");
  host.querySelector<HTMLButtonElement>('[aria-label="Dismiss"]')!.click();
  expect(host.querySelector('[role="status"]')).toBeNull();
});

it("does not revive a decoded image after its original lease was invalidated", async () => {
  await mounted(); const file = image();
  let complete!: (value: ArrayBuffer) => void;
  vi.spyOn(file, "arrayBuffer").mockImplementation(() => new Promise((resolve) => { complete = resolve; }));
  paste({ files: [file] as unknown as FileList });
  fixture.imageCurrent = false;
  complete(png().buffer); await settle();
  expect(fixture.terminalImage).not.toHaveBeenCalled(); expect(fixture.image).not.toHaveBeenCalled();
});

it("keeps text-only paste on the original terminal path", () => {
  const text = "plain\r\ntext";
  expect(paste({ getData: (type) => type === "text/plain" ? text : "" }).defaultPrevented).toBe(true);
  expect(fixture.paste).toHaveBeenCalledExactlyOnceWith(text);
  expect(fixture.image).not.toHaveBeenCalled(); expect(detailTab()).toBe("terminal");
});

it("removes the image paste listener with the original tab", () => {
  const old = host.querySelector(".term-host")!;
  dispose!(); dispose = undefined;
  paste({ files: [image()] as unknown as FileList }, old);
  expect(fixture.image).not.toHaveBeenCalled(); expect(fixture.paste).not.toHaveBeenCalled();
});

/** The tab loads xterm with dynamic imports; its handlers exist once they settle. */
const mounted = async () => { await vi.dynamicImportSettled(); await settle(); };

function rightClick(shiftKey = false) {
  const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 50, shiftKey });
  host.querySelector(".term-host")!.dispatchEvent(event);
  return event;
}
const entry = (label: string) => menu()?.items.find((item) => "label" in item && item.label === label) as { disabled?: boolean; action?: () => void } | undefined;

it("right-click offers Copy selection and Paste, copying the selection exactly and pasting through the terminal", async () => {
  await mounted();
  fixture.selection = "line one\nline two";
  expect(rightClick().defaultPrevented).toBe(true);
  expect(entry("Copy selection")?.disabled).toBeFalsy();
  entry("Copy selection")!.action!();
  expect(fixture.copy).toHaveBeenCalledExactlyOnceWith("line one\nline two");
  entry("Paste")!.action!();
  await settle();
  expect(fixture.clipboard).toHaveBeenCalledTimes(1);
  expect(fixture.paste).toHaveBeenCalledExactlyOnceWith("pasted from the menu", 7);
  expect(fixture.input).not.toHaveBeenCalled();
});

it("right-click without a selection disables Copy selection; an agent reading the mouse gets the click unless Shift is held", async () => {
  await mounted();
  rightClick();
  expect(entry("Copy selection")?.disabled).toBe(true);
  setMenu(undefined);
  fixture.mouse = "any";
  expect(rightClick().defaultPrevented).toBe(true);
  expect(menu()).toBeUndefined();
  rightClick(true);
  expect(entry("Paste")).toBeDefined();
});

it("Ctrl+Shift+C copies the selection without typing, and Ctrl+C stays the agent's", async () => {
  await mounted();
  fixture.selection = "selected output";
  const press = (init: KeyboardEventInit) => fixture.keys!(new KeyboardEvent("keydown", { key: "c", code: "KeyC", ...init }));
  expect(press({ key: "C", ctrlKey: true, shiftKey: true })).toBe(false);
  expect(fixture.copy).toHaveBeenCalledExactlyOnceWith("selected output");
  expect(press({ ctrlKey: true })).toBe(true);
  expect(fixture.copy).toHaveBeenCalledTimes(1);
});

it.each([["during the clipboard read", false], ["before Paste is clicked", true]] as const)(
  "a menu Paste never reaches the session the retained tab moved to %s", async (_when, beforeClick) => {
  // review-win-c's controls for PR #1773 (zerus-q4ayu), with the production terminal store.
  await mounted();
  const actual = await vi.importActual<typeof import("../../store/terminal")>("../../store/terminal");
  const writes: { generation: number; text: string }[] = [];
  let generation = 100;
  const store = actual.createTerminal({
    connect: async (_view, target) => ({ generation: ++generation, status: target.run_id, reused: false }),
    write: async (_view, at, bytes) => { writes.push({ generation: at, text: new TextDecoder().decode(bytes) }); },
    resize: async () => {}, ack() {}, disconnect: async () => null,
    retarget: async () => true, shown() {},
  }, undefined, { hidden: () => false, onChange: () => () => {} });
  const screen = (fixture.show.mock.calls as unknown as [import("../../store/terminal").Screen][]).at(-1)![0];
  fixture.generation = () => store.generation();
  fixture.paste.mockImplementation((text: string, attach?: number) => store.paste(text, attach));
  fixture.input.mockImplementation((bytes: Uint8Array, source: import("../../store/terminal").Screen) => store.input(bytes, source));
  const ok = { supported: true, available: true, reason: "", idle: "idle", connecting: "connecting", disconnected: "disconnected" };
  await store.setTarget({ computer: "fixture", name: "claude/project/alpha", run_id: "alpha", machine: "fixture" }, ok);
  store.show(screen); await settle();
  expect(store.connected()).toBe(true);
  let complete!: (text: string) => void;
  fixture.clipboard.mockImplementationOnce(() => new Promise<string>((resolve) => { complete = resolve; }));
  rightClick();
  const paste = entry("Paste")!.action!;
  const beta = async () => {
    await store.setTarget({ computer: "fixture", name: "claude/project/beta", run_id: "beta", machine: "fixture" }, ok);
    await settle();
    expect(store.generation()).toBe(102);
  };
  if (beforeClick) await beta();
  paste();
  if (!beforeClick) await beta();
  // The person moved on to another field while the clipboard was read (zerus-mp3c9).
  const newer = document.createElement("input"); document.body.append(newer); newer.focus();
  if (complete) complete("alpha-only clipboard text");
  await settle();
  expect(writes).toEqual([]);
  expect(document.activeElement).toBe(newer);
  // A menu opened on the current attach still pastes into it and focuses Terminal.
  rightClick(); entry("Paste")!.action!(); await settle();
  expect(writes).toEqual([{ generation: 102, text: "pasted from the menu" }]);
  expect(document.activeElement).toBe(host.querySelector(".term-host textarea"));
});
