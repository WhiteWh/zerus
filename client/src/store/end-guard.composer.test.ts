import { beforeEach, describe, expect, it, vi } from "vitest";
// The wiring reads the page's query (`?select=`, store/ui.ts); the tests run without a page.
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });
// The undo window, answered at once: each capture gets its delay reference.
const scheduled = vi.hoisted(() => [] as unknown[][]);
vi.mock("./endActions", () => ({
  endActions: {
    schedule: async (captures: unknown[], run: (refs: { id: string; index: number }[], key: string) => Promise<unknown>) => {
      scheduled.push(captures);
      return run(captures.map((_, index) => ({ id: "delay-clear", index })), "test-end");
    },
  },
}));
// The app composer holds these bridge calls from when it loads.
const native = vi.hoisted(() => ({ open: vi.fn(), clear: vi.fn() }));
vi.mock("../bridge", async (importOriginal) => ({ ...(await importOriginal<typeof import("../bridge")>()), composerOpen: native.open, clearContext: native.clear }));
import { composer } from "./composer";
import { applyBatch } from "./catalog";
import { emptyBatch, type ComposerView, type SessionRef } from "../types";

// zerus-sa9qn.27 (#2099 review): the composer's Clear session, like every
// other end action the person starts, reaches Rust with a delay reference and
// the listed run and conversation, never as an undelayed clear.
const SESSION: SessionRef = { computer: "studio", key: "run:run-1", name: "claude/sample-project/review", archive_id: null };
const view = (): ComposerView => ({
  handle: 1, key: "studio\nclaude/sample-project/review", loaded: true, rev: 0, text_epoch: 1, text: "", position: 0, anchor: 0, question: null, input_visible: true, queue: null,
  attachments: [], removed: null, attaching: false, sending: false, waiting: null, uncertain: false, status: "", status_error: false,
  can_send: true, send_label: { source: "Send" }, available: true, unavailable_reason: "", block_reason: null, attachments_available: true,
  attachments_reason: "", session_status: "", session_status_error: false, session_blocks_input: false, inspect_error: false,
  cache: { visible: false, tone: "quiet", label: "", short_label: "" }, cache_warning: "", cache_details: "",
  context: { visible: false, text: "", tooltip: "", warning: false, tone: "quiet", percent: null, accessible_name: "" }, local: [],
  cancel_send: false, compacting: false,
  clear: { enabled: true, dismiss: false, title: "Clear session?", text: "", confirm: "Clear session", run: "run-1", conversation: "conv-1",
    cache_enabled: true, running: false, reason: "", cache_reason: "" },
});

beforeEach(() => {
  scheduled.length = 0;
  native.open.mockReset().mockResolvedValue(view());
  native.clear.mockReset().mockResolvedValue({ kind: "sending" });
});

describe("the composer's Clear session carries a delay reference", () => {
  it("with the run and conversation the session is listed with", async () => {
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "studio", key: "run:run-1", row: {
      name: SESSION.name, agent: "claude", state: "running", tracked: true, resumable: true, activity: "idle", machine: "studio",
      run_id: "run-1", conversation_id: "conv-1",
    } }] });
    await composer.open(SESSION);
    composer.openClear();
    await composer.confirmClear();
    expect(native.clear.mock.calls).toEqual([[1, "run-1", "conv-1", false, { id: "delay-clear", index: 0 }]]);
    expect(scheduled).toEqual([[{
      target: { computer: "studio", name: SESSION.name, run_id: "run-1", conversation_id: "conv-1" },
      action: { kind: "clear_context", dismiss_prompt: false }, state: "running",
    }]]);
  });
});
