// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createComponent, createSignal, Show } from "solid-js";
import { render } from "solid-js/web";
import type { ActionRow } from "./actions-ui";

const calls = vi.hoisted(() => ({ offer: vi.fn(), cancel: vi.fn(), end: vi.fn(), notify: vi.fn() }));
vi.mock("../../bridge", () => ({ terminationCleanupOffer: calls.offer, terminationCleanupCancel: calls.cancel }));
vi.mock("../../store/actions", () => ({
  GONE: "gone", change: vi.fn(), copyCommand: vi.fn(), copyFolder: vi.fn(), copyName: vi.fn(), copyReference: vi.fn(),
  end: calls.end, forget: vi.fn(), interrupting: () => false, notify: calls.notify, perform: vi.fn(), stillListed: () => true,
  pendingNotifications: () => ({ total: 0, conversation: false }), toggleMute: vi.fn(), updating: () => false,
  targetOf: (row: ActionRow) => ({ computer: row.computer, name: row.name, run_id: row.runId ?? "", conversation_id: row.conversationId ?? "" }),
}));
vi.mock("../../store/batch", () => ({ bindBatchConfirm: vi.fn(), endAfter: vi.fn(), running: () => false }));
vi.mock("../../store/endChildren", () => ({ childOfferFor: () => ({ ending: [], staying: [] }) }));
vi.mock("../../store/settings", () => ({ setSetting: vi.fn() }));
vi.mock("../../store/sessionForks", () => ({ sessionForks: {} }));
vi.mock("../../store/restores", () => ({ cancelRestoreFollow: vi.fn(), forgetArchived: vi.fn(), refreshArchiveWhenDone: vi.fn(), restoreSession: vi.fn() }));
vi.mock("../../i18n", () => ({ trNoop: (s: string) => s, tr: (s: string, ...args: unknown[]) => s.replace(/%(\d+)/g, (_, n) => String(args[Number(n) - 1])) }));
vi.mock("../lib/icons", () => ({ Icon: () => null }));

import { TerminateDialog } from "./ActionDialogs";
import { toSession } from "../../store/catalog";
import { snapshotOf } from "./actions-ui";

let dispose: (() => void) | undefined;
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
beforeEach(() => { vi.clearAllMocks(); calls.offer.mockRejectedValue("Unexpected worktree check"); calls.end.mockResolvedValue({ outcome: "done" }); });
afterEach(() => { dispose?.(); dispose = undefined; document.body.replaceChildren(); });

function mount(gitWorktree: boolean | undefined, confirmed: boolean) {
  const row = snapshotOf(toSession("", "run:original", {
    name: "codex/example/fresh", agent: "codex", state: "running", tracked: true,
    run_id: "original-run", conversation_id: confirmed ? "conversation" : null,
    resumable: confirmed, activity: confirmed ? "idle" : "unknown", machine: "fixture", git_worktree: gitWorktree,
  }));
  const host = document.createElement("div"); document.body.append(host);
  const [open, setOpen] = createSignal<ActionRow | undefined>(row);
  dispose = render(() => createComponent(Show, { get when() { return open(); }, keyed: true,
    children: (captured) => createComponent(TerminateDialog, { row: captured as ActionRow, close: () => setOpen(undefined) }),
  }), host);
  return row;
}

it.each([false, true])("ordinary folder has no cleanup check, warning or review and submits its original Terminate once (confirmed %s)", async (confirmed) => {
  const row = mount(false, confirmed); await flush();
  expect(calls.offer).not.toHaveBeenCalled();
  expect(document.querySelector('input[type="checkbox"]')).toBeNull();
  expect(document.querySelector(".cleanup-offer-notice")).toBeNull();
  expect(document.body.textContent).not.toContain("Review working folder");
  const stop = [...document.querySelectorAll("button")].find((button) => button.textContent === "Stop and archive")!;
  expect(stop).toBeDefined(); stop.click(); await flush();
  expect(calls.end).toHaveBeenCalledExactlyOnceWith(row, "terminate", undefined);
  expect(document.body.textContent).not.toContain("Terminate session?");
});

it.each([undefined, true])("unreported/linked metadata still checks the original offer (%s)", async (worktree) => {
  calls.offer.mockResolvedValue(null);
  mount(worktree, true); await flush();
  expect(calls.offer).toHaveBeenCalledExactlyOnceWith({ computer: "", name: "codex/example/fresh", run_id: "original-run", conversation_id: "conversation" });
  expect(calls.end).not.toHaveBeenCalled();
});

it.each([undefined, true])("no cleanup request before its required native conversation exists (%s)", async (worktree) => {
  mount(worktree, false); await flush();
  expect(calls.offer).not.toHaveBeenCalled();
  expect(document.querySelector(".cleanup-offer-notice")).toBeNull();
  expect(document.querySelector('input[type="checkbox"]')).toBeNull();
});
