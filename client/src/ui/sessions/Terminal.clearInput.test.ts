// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import type { ComposerView } from "../../types";

// zerus-wkj9l.4 (M13): the Terminal tab hands Clear input the store's own
// predicate, so a delivery that starts while the confirmation is open withdraws
// the offer and nothing is cleared. The real tab, store and dialog run; only the
// bridge, the composer store, the selected session and xterm are fixtures.
const fixture = vi.hoisted(() => ({
  read: vi.fn(async () => ({ binding: "b".repeat(64), state: "text", text: "stale residue", clearable: true, reason: "" })),
  clear: vi.fn(async () => ({ status: "cleared", error: "", composer: null })),
  toast: vi.fn(),
  view: undefined as unknown,
  session: {
    key: "\nrun:claude/p/a", catalogKey: "run:claude/p/a", computer: "", name: "claude/p/a", sessionId: "6f1c2a4e-1d2b-4c3d-8e4f-5a6b7c8d9e0f",
    status: "ready", state: "running", runId: "run-a", conversationId: "conversation-a", cmd: "claude",
  },
}));
vi.mock("../../bridge", async (importOriginal) => ({ ...(await importOriginal<typeof import("../../bridge")>()),
  composerState: fixture.read, clearInput: fixture.clear }));
vi.mock("../../store/composer", () => ({ composer: { canAttach: () => true, pasteImage: async () => {}, send: () => {}, view: () => fixture.view } }));
vi.mock("../../store/ui", async (importOriginal) => ({ ...(await importOriginal<typeof import("../../store/ui")>()),
  selected: () => fixture.session, toast: fixture.toast }));
vi.mock("../../store/terminal", () => ({
  screenShowsLatest: () => true,
  terminal: { paste() {}, input() {}, show() {}, hide() {}, generation: () => 1,
    captureImage: () => ({ current: () => false, paste: async () => {} }),
    connected: () => false, ready: () => false, texts: () => undefined, status: () => "Fixture terminal", resized() {} },
}));
vi.mock("@xterm/xterm", () => ({ Terminal: class {
  cols = 80; rows = 24; options: Record<string, unknown>; textarea?: HTMLTextAreaElement;
  constructor(options: Record<string, unknown>) { this.options = options; }
  loadAddon() {} onData() {} onBinary() {} onResize() {} write(_bytes: unknown, done?: () => void) { done?.(); }
  attachCustomKeyEventHandler() {} getSelection() { return ""; } hasSelection() { return false; }
  get modes() { return { mouseTrackingMode: "none" }; }
  open(host: HTMLElement) { this.textarea = document.createElement("textarea"); host.append(this.textarea); }
  focus() {} dispose() { this.textarea?.remove(); } reset() {} paste() {}
} }));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit() {} } }));

import { TerminalTab } from "./Terminal";
import { setDetailTab } from "../../store/ui";
import { draftKey } from "../../store/draftKeys";

let dispose: (() => void) | undefined, host: HTMLDivElement;
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
const sending = () => ({
  key: draftKey({ computer: "", key: fixture.session.catalogKey, name: fixture.session.name, archive_id: null, session_id: fixture.session.sessionId }),
  sending: true, delivery_pending: false, waiting: null, local: [],
}) as unknown as ComposerView;

beforeEach(async () => {
  vi.clearAllMocks();
  fixture.view = undefined;
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  if (!HTMLDialogElement.prototype.showModal) HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  setDetailTab("terminal");
  host = document.createElement("div"); document.body.append(host);
  dispose = render(() => createComponent(TerminalTab, {}), host);
  await settle();
});
afterEach(() => { dispose?.(); dispose = undefined; document.body.replaceChildren(); vi.unstubAllGlobals(); });

async function confirmClear() {
  const button = host.querySelector<HTMLButtonElement>(".term-clear-input");
  expect(button, "the read residue is offered").not.toBeNull();
  button!.click();
  await settle();
  const confirm = document.querySelector<HTMLButtonElement>("dialog .btn.solid-danger");
  expect(confirm, "the confirmation shows").not.toBeNull();
  expect(document.querySelector("dialog .clear-input-text")?.textContent).toBe("stale residue");
  return confirm!;
}

it("withdraws the offer when a delivery to the session starts while it is being confirmed", async () => {
  const confirm = await confirmClear();
  fixture.view = sending();
  confirm.click();
  await settle();
  expect(fixture.clear).not.toHaveBeenCalled();
  expect(fixture.toast).toHaveBeenCalledWith("Terminal", expect.stringContaining("a message is being sent"), "error");
});

it("clears exactly the confirmed text when nothing is being sent", async () => {
  const confirm = await confirmClear();
  confirm.click();
  await settle();
  expect(fixture.clear).toHaveBeenCalledExactlyOnceWith(
    { computer: "", name: "claude/p/a", run_id: "run-a", conversation_id: "conversation-a" }, "b".repeat(64), "stale residue");
  expect(fixture.toast).toHaveBeenCalledWith("Terminal", "Input cleared.");
});
