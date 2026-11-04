import { describe, expect, it } from "vitest";
import { createRoot } from "solid-js";
import { createComposer, draftKey, EDIT_MS, flushWhenHidden, MAX_TEXT_BYTES, statusLine, TOO_LONG, type ComposerPort, type Images, type Notices } from "./composer";
import { isEmptyBatch, type ComposerView, type SendReply, type SessionRef } from "../types";
import { normalize } from "../bridge";
import type { Timers } from "./terminal";

const KEY = "studio\nclaude/sample-project/review";
const SESSION: SessionRef = { computer: "studio", key: "run:run-1", name: "claude/sample-project/review", archive_id: null };

const view = (extra: Partial<ComposerView> = {}): ComposerView => ({
  handle: 1, key: KEY, loaded: true, rev: 0, text_epoch: 1, text: "", position: 0, anchor: 0, question: null, input_visible: true, queue: null,
  attachments: [], removed: null, attaching: false, sending: false, waiting: null, uncertain: false, status: "", status_error: false,
  can_send: true, send_label: { source: "Send" }, available: true, unavailable_reason: "", block_reason: null, attachments_available: true,
  attachments_reason: "", session_status: "", session_status_error: false, session_blocks_input: false, inspect_error: false,
  cache: { visible: false, tone: "quiet", label: "", short_label: "" }, cache_warning: "", cache_details: "",
  context: { visible: false, text: "", tooltip: "", warning: false, tone: "quiet", percent: null, accessible_name: "" }, local: [],
  cancel_send: false, compacting: false,
  clear: { enabled: true, dismiss: false, title: "Clear session?", text: "", confirm: "Clear session", run: "run-1", conversation: "conv-1",
    cache_enabled: true, running: false, reason: "", cache_reason: "" },
  ...extra,
});

function fakes(reply: SendReply = { kind: "sending" }, opened: ComposerView | null = view()) {
  const calls: string[] = [];
  const notices: string[] = [];
  let attachDone: (() => void) | undefined;
  let openDone: ((v: ComposerView | null) => void) | undefined;
  let answerReply: SendReply = { kind: "sending" };
  let retryReply: SendReply = { kind: "sending" };
  let holdOpen = false;
  /** What opening a draft attachment answers next: null (opened), a notice, or a thrown error. */
  let openReply: () => string | null = () => null;
  const port: ComposerPort = {
    open: (session) => {
      calls.push(`open ${session?.name ?? "none"}`);
      if (!holdOpen) return Promise.resolve(session ? opened : null);
      holdOpen = false;
      return new Promise((resolve) => { openDone = resolve; });
    },
    edit: async (_h, rev, _epoch, text) => { calls.push(`edit ${rev} ${text}`); },
    pick: () => new Promise<void>((resolve) => { calls.push("pick"); attachDone = resolve; }),
    attach: async (_h, paths, start, end) => { calls.push(`attach ${paths.join(",")} ${start}-${end}`); },
    pasteImage: async (_h, bytes) => { calls.push(`paste ${bytes.length}`); },
    send: async (_h, rev, _epoch, text) => { calls.push(`send ${rev} ${text}`); return reply; },
    push: async (_h, rev, _epoch, text, position, anchor) => { calls.push(`push ${rev} ${text} ${position} ${anchor}`); return reply; },
    sendNow: async (id) => { calls.push(`send-now ${id}`); return true; },
    answer: async (token, answer) => { calls.push(`answer ${token} ${JSON.stringify(answer)}`); return answerReply; },
    remove: async (_h, index) => { calls.push(`remove ${index}`); }, undo: async () => { calls.push("undo"); },
    allowRetry: async () => { calls.push("allow retry"); }, cancelSend: async () => { calls.push("cancel send"); },
    interrupted: async () => { calls.push("interrupted"); },
    clear: async (_h, run, conversation, dismiss) => { calls.push(`clear ${run} ${conversation} ${dismiss}`); return { kind: "notice", text: "Context cleared. Your draft is ready to send.", error: false }; },
    refresh: async (h) => { calls.push(`refresh ${h}`); },
    openAttachment: async (path) => { calls.push(`open file ${path}`); return openReply(); },
    localRetry: async (h, id) => { calls.push(`retry ${h} ${id}`); return retryReply; },
    localDelete: async (h, id) => { calls.push(`delete ${h} ${id}`); },
    localInspect: async (h, id) => { calls.push(`inspect ${h} ${id}`); return { kind: "terminal" }; },
  };
  const notify: Notices = (tone, text, sticky) => { notices.push(`${tone} ${sticky ? "sticky" : "timed"} ${text}`); };
  let clock = 0;
  const jobs: { at: number; fn: () => void }[] = [];
  const timers: Timers = { now: () => clock, later: (fn, ms) => { const j = { at: clock + ms, fn }; jobs.push(j); return () => { const at = jobs.indexOf(j); if (at >= 0) jobs.splice(at, 1); }; } };
  const advance = (ms: number) => { clock += ms; for (const j of jobs.filter((x) => x.at <= clock)) { jobs.splice(jobs.indexOf(j), 1); j.fn(); } };
  return {
    calls, notices, port, timers, advance, notify,
    finishAttach: () => attachDone?.(),
    /** The next open answers only with finishOpen. */
    holdOpens: () => { holdOpen = true; },
    finishOpen: (v: ComposerView | null) => openDone?.(v),
    answerWith: (r: SendReply) => { answerReply = r; },
    openWith: (reply: () => string | null) => { openReply = reply; },
    retryWith: (r: SendReply) => { retryReply = r; },
  };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Calls whose answers the test gives later. */
function held<T>() {
  const waiting: ((value: T) => void)[] = [];
  return { call: () => new Promise<T>((resolve) => { waiting.push(resolve); }), answer: (value: T) => waiting.shift()?.(value), count: () => waiting.length };
}
const images = (width: number, height: number): Images => ({ size: async () => ({ width, height }), png: async () => new Uint8Array([1, 2, 3]) });

describe("child draft identity", () => {
  it("uses Qt's four-part child key without changing main or archive keys", () => {
    expect(draftKey(SESSION)).toBe(KEY);
    expect(draftKey({ ...SESSION, archive_id: "archive-1" })).toBe(`${KEY}\narchive-1`);
    expect(draftKey({ ...SESSION, subagent: { agent_id: "agent-7", parent_conversation_id: "conv-1" } })).toBe(`${KEY}\nconv-1\nagent-7`);
  });
});

describe("composer exit flush", () => {
  it("flushes pending debounce and waits for the edit before returning", async () => {
    const f = fakes(); const writing = held<void>();
    await createRoot(async () => {
      const c = createComposer({ ...f.port, edit: () => writing.call() }, f.timers);
      c.apply(view()); c.input("last keystroke", 14, 14);
      let done = false; const flushing = c.flushForExit().then(() => { done = true; });
      await tick(); expect(writing.count()).toBe(1); expect(done).toBe(false);
      expect(c.locked()).toBe(true); expect(c.pushEnabled()).toBe(false);
      c.input("too late", 8, 8); await c.push(); await c.sendNow();
      expect(c.text()).toBe("last keystroke"); expect(f.calls).toEqual([]);
      writing.answer(); await flushing; expect(done).toBe(true);
      f.advance(EDIT_MS); await tick(); expect(writing.count()).toBe(0);
    });
  });

  it("waits for an edit already sent by blur, including one from the previous composer", async () => {
    const f = fakes(); const writing = held<void>();
    await createRoot(async () => {
      const c = createComposer({ ...f.port, edit: () => writing.call() }, f.timers);
      c.apply(view()); c.input("draft A", 7, 7);
      const blurred = c.flush(); await tick();
      c.apply(view({handle:2,key:"studio\nclaude/example/other",text:"draft B"}));
      let done=false; const flushing=c.flushForExit().then(()=>{done=true;});
      await tick(); expect(done).toBe(false);
      writing.answer(); await blurred; await flushing; expect(done).toBe(true);
    });
  });
});

describe("composer store", () => {
  it("push flushes the field and cursor once through the captured composer", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("also lint", 2, 9);
      await c.push();
      expect(f.calls).toEqual(["edit 1 also lint", "push 1 also lint 9 2"]);
    });
  });

  it.each([false, true])("an empty/sending field (sending=%s) pushes its own handle and waits for Busy hand-back", async (sending) => {
    const f = fakes();
    const captured: unknown[][] = [];
    const port: ComposerPort = { ...f.port, push: async (...args) => { captured.push(args); return { kind: "busy" }; } };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      const text = sending ? "old message" : "";
      c.apply(view({ handle: 7, rev: 4, text_epoch: 8, sending, text }));
      await c.push();
      expect(captured).toEqual([[7, 4, 8, text, 0, 0]]);
      expect(f.calls).toEqual([]);
      expect(c.locked()).toBe(true);
      c.apply(view({ handle: 7, text_epoch: 9, text }));
      expect(c.locked()).toBe(false);
    });
  });

  it("a hand-back during the push flush never substitutes its new epoch or text", async () => {
    const f = fakes();
    const edit = held<void>();
    const captured: unknown[][] = [];
    const port: ComposerPort = { ...f.port, edit: () => edit.call(), push: async (...args) => { captured.push(args); return { kind: "busy" }; } };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      c.apply(view());
      c.input("also lint", 9, 9);
      const pushing = c.push();
      await tick();
      c.apply(view({ text_epoch: 2, text: "handed back", uncertain: true }));
      edit.answer();
      await pushing;
      expect(captured).toEqual([[1, 1, 1, "also lint", 9, 9]]);
      expect(c.text()).toBe("handed back");
      expect(c.locked()).toBe(false);
    });
  });

  it.each(["busy", "blocked"] as const)("a %s push waits for its text hand-back and then unlocks", async (kind) => {
    const f = fakes(kind === "busy" ? { kind } : { kind, reason: "Wait for the session update to finish." });
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ text: "hello" }));
      await c.push();
      expect(c.locked()).toBe(true);
      c.apply(view({ text: "hello", text_epoch: 2 }));
      expect(c.locked()).toBe(false);
      expect(c.text()).toBe("hello");
      expect(f.calls).toEqual(["push 0 hello 0 0"]);
    });
  });

  it("a required card refuses push before either message or queue dispatch", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      for (const sending of [false, true]) {
        c.apply(view({ input_visible: false, sending, text: "hello" }));
        expect(c.pushEnabled()).toBe(false);
        await c.push();
      }
      expect(f.calls).toEqual([]);
    });
  });

  it.each([{text: "hello", sending: false}, {text: "", sending: false}, {text: "", sending: true}])("a second push while its bridge call is held sends nothing (%j)", async (field) => {
    const f = fakes();
    const pending = held<SendReply>();
    const port: ComposerPort = { ...f.port, push: () => pending.call() };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      c.apply(view(field));
      const first = c.push();
      await tick();
      expect(pending.count()).toBe(1);
      const second = c.push();
      await tick();
      expect(pending.count()).toBe(1);
      pending.answer({ kind: "sending" });
      await first;
      await second;
    });
  });

  it("a selection changed during the flush never dispatches its push", async () => {
    const f = fakes();
    const edit = held<void>();
    const port: ComposerPort = { ...f.port, edit: () => edit.call() };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      c.apply(view());
      c.input("A's message", 0, 11);
      const pushing = c.push();
      await tick();
      c.apply(view({ handle: 2, key: "studio\nclaude/sample-project/other", text: "B's draft" }));
      edit.answer();
      await pushing;
      expect(f.calls).toEqual([]);
      expect(c.text()).toBe("B's draft");
      expect(c.locked()).toBe(false);
    });
  });

  it("an open that starts during the flush cancels push before its old handle changes", async () => {
    const f = fakes();
    const edit = held<void>();
    const port: ComposerPort = { ...f.port, edit: () => edit.call() };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      c.apply(view());
      c.input("A's message", 0, 11);
      const pushing = c.push();
      await tick();
      f.holdOpens();
      const other: SessionRef = { ...SESSION, key: "run:other", name: "claude/sample-project/other" };
      const opening = c.open(other);
      await tick();
      edit.answer();
      await pushing;
      expect(f.calls).toEqual(["open claude/sample-project/other"]);
      f.finishOpen(view({ handle: 2, key: draftKey(other), text: "B's draft" }));
      await opening;
      expect(c.text()).toBe("B's draft");
      expect(c.locked()).toBe(false);
    });
  });

  it("Send now passes the panel id or null and reports bridge errors", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers, undefined, f.notify);
      c.apply(view());
      await c.sendNow("queue-1");
      await c.sendNow();
      expect(f.calls).toEqual(["send-now queue-1", "send-now null"]);
      const failed = createComposer({ ...f.port, sendNow: async () => { throw "Bridge unavailable"; } }, f.timers, undefined, f.notify);
      await failed.sendNow();
      expect(f.notices).toEqual(["error sticky Bridge unavailable"]);
    });
  });

  it("a window Send now during a composer switch never uses the previous session", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers, undefined, f.notify);
      const queue = { id: "queue-a", text: "A's queue", hint: "", can_send_now: true, enabled: true, label: "Send now" };
      c.apply(view({ queue }));
      expect(c.queue()).toEqual(queue);
      const other: SessionRef = { ...SESSION, key: "run:other", name: "claude/sample-project/other" };
      f.holdOpens();
      const opening = c.open(other);
      await tick();
      expect(c.queue()).toBeNull();
      await c.sendNow();
      expect(f.calls).toEqual(["open claude/sample-project/other"]);
      expect(f.notices).toEqual(["info timed Nothing queued can be sent now"]);
      expect(c.pushEnabled()).toBe(false);
      f.finishOpen(view({ handle: 2, key: draftKey(other), queue: { ...queue, id: "queue-b", text: "B's queue" } }));
      await opening;
      expect(c.queue()?.id).toBe("queue-b");
      await c.sendNow("queue-b");
      expect(f.calls.at(-1)).toBe("send-now queue-b");
    });
  });

  it("edits are debounced and flushed before a send", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("h", 1, 1); c.input("he", 2, 2); c.input("hel", 3, 3);
      f.advance(EDIT_MS);
      await tick();
      expect(f.calls).toEqual(["edit 3 hel"]);
      c.input("hello", 5, 5);
      await c.send();
      expect(f.calls.slice(1)).toEqual(["edit 4 hello", "send 4 hello"]);
    });
  });

  it("edits and sends carry the epoch of the text they are based on", async () => {
    const f = fakes();
    const epochs: number[] = [];
    const port: ComposerPort = {
      ...f.port,
      edit: async (_h, _rev, epoch) => { epochs.push(epoch); },
      send: async (_h, _rev, epoch) => { epochs.push(epoch); return { kind: "sending" }; },
    };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      c.apply(view({ text_epoch: 4 }));
      c.input("hello", 5, 5);
      await c.send();
      expect(epochs).toEqual([4, 4]);
    });
  });

  it("remove and Undo lock the editor until a view with a higher epoch arrives", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ text: "a [File #1] ", text_epoch: 2 }));
      await c.remove(0);
      expect(c.locked()).toBe(true);
      c.input("typed while locked", 18, 18);
      expect(c.text()).toBe("a [File #1] ");
      c.apply(view({ text: "a  ", text_epoch: 2 }));
      expect(c.locked()).toBe(true);
      c.apply(view({ text: "a  ", text_epoch: 3 }));
      expect(c.locked()).toBe(false);
      expect(c.text()).toBe("a  ");
      await c.undo();
      expect(c.locked()).toBe(true);
      c.apply(view({ text: "a  [File #1] ", text_epoch: 4 }));
      expect(c.locked()).toBe(false);
    });
  });

  it("a higher text epoch replaces the text; an older echo does not", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ text: "loaded", text_epoch: 2, rev: 0 }));
      expect(c.text()).toBe("loaded");
      c.input("typing", 6, 6);
      c.apply(view({ text: "loaded", text_epoch: 2, rev: 0 }));
      expect(c.text()).toBe("typing");
      c.apply(view({ text: "loaded [File #1] ", text_epoch: 3, rev: 1 }));
      expect(c.text()).toBe("loaded [File #1] ");
    });
  });

  it("a required card disables Send and ignores a toolbar chord without a hint", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ input_visible: false, text: "draft", status: "Answer the question to continue." }));
      expect(c.sendEnabled()).toBe(false);
      await c.send();
      expect(f.calls).toEqual([]);
      expect(c.hint()).toBeUndefined();
      expect(c.text()).toBe("draft");
      c.apply(view({ text: "draft" }));
      expect(c.sendEnabled()).toBe(true);
      await c.send();
      expect(f.calls.filter((call) => call.startsWith("send "))).toHaveLength(1);
    });
  });

  it("over 64 KiB disables Send at once and shows Qt's text", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("ж".repeat(32 * 1024 + 1), 0, 0);
      expect(c.sendEnabled()).toBe(false);
      expect(c.status()).toEqual({ text: "Message exceeds 64 KiB.", error: true });
    });
  });

  it("one attach at a time, and the editor is read-only meanwhile", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      const first = c.attachFiles();
      await tick();
      expect(c.locked()).toBe(true);
      await c.attachFiles();
      expect(f.calls.filter((x) => x === "pick").length).toBe(1);
      f.finishAttach();
      await first;
      expect(c.locked()).toBe(false);
    });
  });

  // Qt refuses above 32 000 000 pixels (MessageComposer.cpp:394), as the core does.
  it("a non-PNG paste above 32 megapixels is refused before any call", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers, images(8000, 4001));
      c.apply(view());
      await c.pasteImage(new Blob([new Uint8Array([0xff, 0xd8])], { type: "image/jpeg" }));
      expect(f.calls.some((x) => x.startsWith("paste"))).toBe(false);
      expect(c.status()).toEqual({ text: "Image is too large. Use an image below 32 megapixels.", error: true });
      // Typing keeps the error, as Qt's showError notice (cleared by a hand-back, a send or Allow retry).
      c.input("x", 1, 1);
      expect(c.status().text).toBe("Image is too large. Use an image below 32 megapixels.");
      const exact = createComposer(f.port, f.timers, images(8000, 4000));
      exact.apply(view());
      await exact.pasteImage(new Blob([new Uint8Array([0xff, 0xd8])], { type: "image/jpeg" }));
      expect(f.calls.at(-1)).toBe("paste 3");
    });
  });

  it("a cold reply opens the question and its answer goes back with the token", async () => {
    const f = fakes({ kind: "cold", token: 9, prompt: { title: "Continue with a cold cache?", text: "t", informative: "", choices: [] } });
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("hi", 2, 2);
      await c.send();
      expect(c.dialog()?.kind).toBe("cold");
      await c.answer({ kind: "cold", action: "send" });
      expect(f.calls.at(-1)).toBe(`answer 9 {"kind":"cold","action":"send"}`);
      expect(c.dialog()).toBeUndefined();
    });
  });

  it("an open question locks the editor and keeps the epoch: closing it unlocks", async () => {
    const f = fakes({ kind: "start", token: 3, question: { title: { source: "Start the session to send?" }, text: { source: "" }, informative: { source: "" }, options: [] } });
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("hi", 2, 2);
      await c.send();
      expect(c.dialog()?.kind).toBe("start");
      expect(c.locked()).toBe(true);
      c.closeDialog();
      await tick();
      expect(f.calls.at(-1)).toBe(`answer 3 {"kind":"cancel"}`);
      expect(c.locked()).toBe(false);
      expect(c.text()).toBe("hi");
    });
  });

  it("another composer, or none, cancels an open question", async () => {
    const f = fakes({ kind: "cold", token: 6, prompt: { title: "", text: "", informative: "", choices: [] } });
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("hi", 2, 2);
      await c.send();
      expect(c.dialog()?.kind).toBe("cold");
      c.apply(view({ handle: 2, key: "studio\nclaude/sample-project/other" }));
      await tick();
      expect(c.dialog()).toBeUndefined();
      expect(f.calls.at(-1)).toBe(`answer 6 {"kind":"cancel"}`);
      expect(c.locked()).toBe(false);
    });
  });

  it("a refused send stays locked until Rust hands the text back; a notice unlocks at once", async () => {
    const f = fakes({ kind: "busy" });
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers, undefined, f.notify);
      c.apply(view({ text_epoch: 2 }));
      c.input("hi", 2, 2);
      await c.send();
      expect(c.locked()).toBe(true);
      c.apply(view({ text: "hi", text_epoch: 3 }));
      expect(c.locked()).toBe(false);
    });
    const n = fakes({ kind: "notice", text: "Wait for the session update to finish.", error: true });
    await createRoot(async () => {
      const c = createComposer(n.port, n.timers, undefined, n.notify);
      c.apply(view());
      c.input("hi", 2, 2);
      await c.send();
      expect(c.locked()).toBe(false);
      expect(n.notices).toEqual(["error sticky Wait for the session update to finish."]);
    });
  });

  it("a started send stays locked until the view shows it sending", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("hi", 2, 2);
      await c.send();
      expect(c.locked()).toBe(true);
      c.apply(view({ sending: true }));
      c.apply(view({ sending: true, text_epoch: 2 }));
      expect(c.locked()).toBe(true);
      c.apply(view({ text_epoch: 2 }));
      expect(c.locked()).toBe(false);
    });
  });

  // Qt's showNotice(text, error): only an error stays (SessionsWindow.cpp:5210).
  it("notices take Qt's tone: an error stays, a plain notice goes", async () => {
    const f = fakes({ kind: "cold", token: 4, prompt: { title: "", text: "", informative: "", choices: [] } });
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers, undefined, f.notify);
      c.apply(view());
      c.input("hi", 2, 2);
      await c.send();
      f.answerWith({ kind: "notice", text: "Draft changed. Send it when you are ready.", error: false });
      await c.answer({ kind: "cold", action: "send" });
      expect(f.notices).toEqual(["info timed Draft changed. Send it when you are ready."]);
    });
  });

  it("Clear context from the cold question opens the Clear dialog, which sends back the identity it showed", async () => {
    const f = fakes({ kind: "cold", token: 5, prompt: { title: "", text: "", informative: "", choices: [] } });
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers, undefined, f.notify);
      c.apply(view());
      c.input("hi", 2, 2);
      await c.send();
      f.answerWith({ kind: "clear_context", token: 5 });
      await c.answer({ kind: "cold", action: "clear" });
      expect(c.dialog()?.kind).toBe("clear");
      expect(c.locked()).toBe(false);
      // The session's run changed meanwhile: the dialog still names the one it showed.
      c.apply(view({ clear: { ...view().clear, run: "run-2" } }));
      const confirmed = c.confirmClear();
      expect(c.dialog()).toBeUndefined();
      await confirmed;
      expect(f.calls.at(-1)).toBe("clear run-1 conv-1 false");
      expect(f.notices).toEqual(["info timed Context cleared. Your draft is ready to send."]);
      // Closing it answers nothing.
      c.openClear();
      c.closeDialog();
      await tick();
      expect(f.calls.at(-1)).toBe("clear run-1 conv-1 false");
      // A clear that is not allowed opens no dialog.
      c.apply(view({ clear: { ...view().clear, enabled: false } }));
      c.openClear();
      expect(c.dialog()).toBeUndefined();
    });
  });

  // The Task 4b review ruling: Rust may change the text itself in these states.
  it("with an error, an uncertain mark or a failed local row every key is flushed at once", async () => {
    for (const extra of [{ status_error: true, status: "Delivery failed." }, { uncertain: true }, {
      local: [{ id: "r", text: "x", status: "error" as const, attachments: [], run_id: "run-1", conversation_id: "conv-1", submitted_at: 1, error: "e",
        uncertain: false, message_id: null, submitted_text: null, source: null, voice: null }],
    }]) {
      const f = fakes();
      await createRoot(async () => {
        const c = createComposer(f.port, f.timers);
        c.apply(view(extra));
        c.input("a", 1, 1);
        await tick();
        expect(f.calls).toEqual(["edit 1 a"]);
      });
    }
  });

  it("an envelope view of another composer, or one older than an open under way, is not shown", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      await c.open(SESSION);
      expect(c.view()?.handle).toBe(1);
      // A view taken before the open, of the session shown before.
      c.receive(view({ handle: 7, key: "studio\nclaude/sample-project/other", text: "other", text_epoch: 9 }));
      expect(c.text()).toBe("");
      c.receive(view({ text: "loaded", text_epoch: 2 }));
      expect(c.text()).toBe("loaded");
      // While an open runs, envelope views wait for its answer.
      f.holdOpens();
      const other = { ...SESSION, key: "run:run-9", name: "claude/sample-project/other" };
      const opening = c.open(other);
      await tick();
      c.receive(view({ text: "stale", text_epoch: 3 }));
      expect(c.text()).toBe("loaded");
      f.finishOpen(view({ handle: 8, key: draftKey(other), text: "other draft", text_epoch: 1 }));
      await opening;
      expect(c.view()?.handle).toBe(8);
      expect(c.text()).toBe("other draft");
      // No session: nothing is shown and nothing taken.
      await c.open(undefined);
      expect(c.view()).toBeUndefined();
      c.receive(view({ handle: 8, key: draftKey(other), text_epoch: 5 }));
      expect(c.view()).toBeUndefined();
    });
  });

  // Review fix round 1, I1 (probe P5): a reply arrives after another composer opened.
  it("a clear that ends after the switch to another composer neither locks nor changes it; its notice still shows", async () => {
    const f = fakes();
    const clear = held<SendReply>();
    const port: ComposerPort = { ...f.port, clear: () => clear.call() };
    await createRoot(async () => {
      const c = createComposer(port, f.timers, undefined, f.notify);
      c.apply(view());
      c.openClear();
      const confirmed = c.confirmClear();
      c.apply(view({ handle: 2, key: "studio\nclaude/sample-project/other" }));
      clear.answer({ kind: "sending" });
      await confirmed;
      expect(c.locked()).toBe(false);
      c.openClear();
      const again = c.confirmClear();
      c.apply(view({ handle: 3, key: "studio\nclaude/sample-project/third" }));
      clear.answer({ kind: "notice", text: "Context cleared. Your draft is ready to send.", error: false });
      await again;
      expect(f.notices).toEqual(["info timed Context cleared. Your draft is ready to send."]);
      expect(c.locked()).toBe(false);
    });
  });

  // I1 (probe P6): a question for the composer left is answered Cancel, never shown over another.
  it("a cold or start question that arrives after the switch is answered Cancel and not shown", async () => {
    const f = fakes();
    const send = held<SendReply>();
    const port: ComposerPort = { ...f.port, send: (_h, rev, _epoch, text) => { f.calls.push(`send ${rev} ${text}`); return send.call(); } };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      c.apply(view());
      c.input("for A", 5, 5);
      const sent = c.send();
      await tick();
      c.apply(view({ handle: 2, key: "studio\nclaude/sample-project/other", text: "B's draft", text_epoch: 1 }));
      send.answer({ kind: "cold", token: 11, prompt: { title: "Continue with a cold cache?", text: "", informative: "", choices: [] } });
      await sent;
      await tick();
      expect(c.dialog()).toBeUndefined();
      expect(f.calls.at(-1)).toBe(`answer 11 {"kind":"cancel"}`);
      expect(c.text()).toBe("B's draft");
      expect(c.locked()).toBe(false);
      // A busy, blocked or sending reply for the composer left changes nothing either.
      c.input("for B", 5, 5);
      const second = c.send();
      await tick();
      c.apply(view({ handle: 4, key: "studio\nclaude/sample-project/fourth", text_epoch: 1 }));
      send.answer({ kind: "busy" });
      await second;
      expect(c.locked()).toBe(false);
    });
  });

  it("the editor stays locked while an answer is on its way", async () => {
    const f = fakes({ kind: "start", token: 12, question: { title: { source: "Start the session to send?" }, text: { source: "" }, informative: { source: "" }, options: [] } });
    const answer = held<SendReply>();
    const port: ComposerPort = { ...f.port, answer: (token, a) => { f.calls.push(`answer ${token} ${JSON.stringify(a)}`); return answer.call(); } };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      c.apply(view());
      c.input("hi", 2, 2);
      await c.send();
      const answering = c.answer({ kind: "start", choice: "resume" });
      expect(c.dialog()).toBeUndefined();
      expect(c.locked()).toBe(true);
      await tick();
      answer.answer({ kind: "busy" });
      await answering;
      expect(c.locked()).toBe(false);
    });
  });

  // Fix round 2: the call locks are per session (the reviewer's N9).
  it("a call locks only its own composer, also after going to another one and back", async () => {
    const f = fakes();
    const forA = held<SendReply>(), forB = held<SendReply>();
    const port: ComposerPort = { ...f.port, send: (h, rev, _epoch, text) => { f.calls.push(`send ${h} ${text}`); return h === 1 ? forA.call() : forB.call(); } };
    const A = view({ handle: 1 }), B = view({ handle: 2, key: "studio\nclaude/sample-project/other" });
    await createRoot(async () => {
      const c = createComposer(port, f.timers, undefined, f.notify);
      c.apply(A);
      c.input("for A", 5, 5);
      const sentA = c.send();
      await tick();
      expect(c.locked()).toBe(true);
      c.apply(B);
      expect(c.locked()).toBe(false);
      c.input("for B", 5, 5);
      const sentB = c.send();
      await tick();
      expect(c.locked()).toBe(true);
      // Back on A while both calls run: A's own call still locks it, also once B's ends.
      c.apply(A);
      expect(c.locked()).toBe(true);
      forB.answer({ kind: "busy" });
      await sentB;
      expect(c.locked()).toBe(true);
      forA.answer({ kind: "notice", text: "Wait for the session update to finish.", error: true });
      await sentA;
      expect(c.locked()).toBe(false);
      expect(f.calls.filter((x) => x.startsWith("send"))).toEqual(["send 1 for A", "send 2 for B"]);
    });
  });

  // M1 (probe P1): a second Enter while the first send flushes sends nothing.
  it("two sends from one Enter: the second finds the editor locked", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("hello", 5, 5);
      const first = c.send();
      const second = c.send();
      await Promise.all([first, second]);
      expect(f.calls.filter((x) => x.startsWith("send"))).toEqual(["send 1 hello"]);
    });
  });

  // M2 (probe P2) and M6: the composer being left gets its typing first, and nothing is typed while another opens.
  it("opening another session flushes the one left first and locks the editor until it answers", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      await c.open(SESSION);
      f.calls.length = 0;
      c.input("typed on A", 10, 10);
      f.holdOpens();
      const other = { ...SESSION, key: "run:run-9", name: "claude/sample-project/other" };
      const opening = c.open(other);
      expect(c.locked()).toBe(true);
      c.input("lost?", 5, 5);
      expect(c.text()).toBe("typed on A");
      await tick();
      expect(f.calls).toEqual(["edit 1 typed on A", "open claude/sample-project/other"]);
      f.finishOpen(view({ handle: 8, key: draftKey(other), text: "B", text_epoch: 1 }));
      await opening;
      expect(c.locked()).toBe(false);
      expect(c.text()).toBe("B");
    });
  });

  // M6: an envelope view held while an open ran is applied after its answer.
  it("a view of the opened composer that arrived during the open is applied afterwards", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      f.holdOpens();
      const opening = c.open(SESSION);
      await tick();
      c.receive(view({ text: "newer", text_epoch: 2, status: "Newer status." }));
      f.finishOpen(view({ text: "older", text_epoch: 1 }));
      await opening;
      expect(c.text()).toBe("newer");
      expect(c.view()?.status).toBe("Newer status.");
    });
  });

  // M6: Qt's Send needs no uncertain mark (MessageComposer.cpp:1185).
  it("an uncertain mark disables Send until Allow retry", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ uncertain: true }));
      c.input("hello", 5, 5);
      expect(c.sendEnabled()).toBe(false);
      await c.send();
      expect(f.calls.some((x) => x.startsWith("send"))).toBe(false);
      c.apply(view());
      expect(c.sendEnabled()).toBe(true);
    });
  });

  // M6: Qt saves the cursor with the draft (rememberCursor, MessageComposer.cpp:380-386).
  it("a cursor move is an edit", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ text: "hello", position: 5, anchor: 5 }));
      c.select(5, 5);
      f.advance(EDIT_MS);
      await tick();
      expect(f.calls).toEqual([]);
      c.select(0, 2);
      f.advance(EDIT_MS);
      await tick();
      expect(f.calls).toEqual(["edit 1 hello"]);
    });
  });

  // M6: Refresh is off while an operation locks the composer (MessageComposer.cpp:1195).
  it("Refresh does nothing while an operation locks the composer", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ sending: true, session_status: "Could not refresh activity: timed out", session_status_error: true }));
      expect(c.operationLocked()).toBe(true);
      await c.refresh();
      expect(f.calls).toEqual([]);
    });
  });

  // M8: typing not yet flushed goes when the window hides or the page goes.
  it("hiding the window or leaving the page flushes the typing", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      let hidden = false;
      const listeners: (() => void)[] = [];
      let pagehide: (() => void) | undefined;
      flushWhenHidden(() => { void c.flush(); }, [{ hidden: () => hidden, onChange: (l) => { listeners.push(l); return () => {}; } }],
        { addEventListener: (_type, listener) => { pagehide = listener; } });
      c.apply(view());
      c.input("a", 1, 1);
      for (const l of listeners) l();
      await tick();
      expect(f.calls).toEqual([]);
      hidden = true;
      for (const l of listeners) l();
      await tick();
      expect(f.calls).toEqual(["edit 1 a"]);
      c.input("ab", 2, 2);
      pagehide?.();
      await tick();
      expect(f.calls).toEqual(["edit 1 a", "edit 2 ab"]);
    });
  });

  // M3: the length is measured once per text, with Qt's UTF-8 rule at the edge.
  it("the 64 KiB limit counts UTF-8 bytes at the edge", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("a".repeat(MAX_TEXT_BYTES), 0, 0);
      expect(c.status().text).toBe("");
      c.input("a".repeat(MAX_TEXT_BYTES + 1), 0, 0);
      expect(c.status().text).toBe(TOO_LONG);
      c.input("ж".repeat(MAX_TEXT_BYTES / 2), 0, 0);
      expect(c.status().text).toBe("");
      c.input(`${"ж".repeat(MAX_TEXT_BYTES / 2 - 1)}€`, 0, 0);
      expect(c.status().text).toBe(TOO_LONG);
    });
  });

  it("Send while disabled shows the status at the editor for 5 s and sends nothing", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ available: false, unavailable_reason: "Respond to the agent in the Terminal tab.", status: "Respond to the agent in the Terminal tab." }));
      c.input("hi", 2, 2);
      await c.send();
      expect(f.calls.some((x) => x.startsWith("send"))).toBe(false);
      expect(c.hint()).toBe("Respond to the agent in the Terminal tab.");
      f.advance(5000);
      expect(c.hint()).toBeUndefined();
    });
  });

  it("the session status locks the editor but not Allow retry or Refresh", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ session_blocks_input: true, session_status: "Machine unavailable.", session_status_error: true, status: "Machine unavailable.", status_error: true }));
      expect(c.locked()).toBe(true);
      expect(c.operationLocked()).toBe(false);
      await c.refresh();
      expect(f.calls).toEqual(["refresh 1"]);
    });
  });
});

describe("dropped files (MessageComposer::attachDroppedFiles, 961-967)", () => {
  it("attach at the cursor, the selection collapsed, after the typing is saved", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view());
      c.input("hello world", 6, 11);
      expect(c.canAttach()).toBe(true);
      await c.attachDropped(["/srv/home/example/notes.txt"]);
      expect(f.calls.slice(-2)).toEqual(["edit 1 hello world", "attach /srv/home/example/notes.txt 11-11"]);
    });
  });

  it("can attach only while the session takes attachments and nothing locks the composer", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ attachments_available: false }));
      expect(c.canAttach()).toBe(false);
      c.apply(view({ sending: true }));
      expect(c.canAttach()).toBe(false);
      await c.attachDropped(["/srv/a.txt"]);
      expect(f.calls.some((call) => call.startsWith("attach"))).toBe(false);
    });
  });
});

describe("opening a draft attachment (MessageComposer::openAttachment, 1067-1085)", () => {
  const shot = { name: "shot.png", mime: "image/png", reference: "[Image #1]", bytes: 8, path: "draft/1/2/0", excerpt: null };

  it("opens by the row's draft path and says nothing when it opened", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ attachments: [shot] }));
      await c.openAttachment(0);
      expect(f.calls).toContain("open file draft/1/2/0");
      expect(c.status()).toEqual({ text: "", error: false });
    });
  });

  it("shows a program shown in the file manager, or a failed open, in the status line", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ attachments: [shot] }));
      f.openWith(() => "setup.exe can run programs, so it is shown in the file manager instead of being opened.");
      await c.openAttachment(0);
      expect(c.status()).toEqual({ text: "setup.exe can run programs, so it is shown in the file manager instead of being opened.", error: true });
      f.openWith(() => { throw "Could not open the attachment with its default application."; });
      await c.openAttachment(0);
      expect(c.status().text).toBe("Could not open the attachment with its default application.");
    });
  });

  it("opens nothing while the draft sends, or for a row that is gone", async () => {
    const f = fakes();
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers);
      c.apply(view({ attachments: [shot], sending: true }));
      await c.openAttachment(0);
      c.apply(view({ attachments: [shot] }));
      await c.openAttachment(3);
      expect(f.calls.filter((call) => call.startsWith("open file"))).toEqual([]);
    });
  });

  it("drops the notice when another composer is open by the time the open answers", async () => {
    const f = fakes();
    let finish!: (notice: string) => void;
    const port: ComposerPort = { ...f.port, openAttachment: () => new Promise((resolve) => { finish = resolve; }) };
    await createRoot(async () => {
      const c = createComposer(port, f.timers);
      c.apply(view({ attachments: [shot] }));
      const opening = c.openAttachment(0);
      c.apply(view({ handle: 2, key: "other" }));
      finish("setup.exe can run programs, so it is shown in the file manager instead of being opened.");
      await opening;
      expect(c.status().text).toBe("");
    });
  });
});

describe("the status line", () => {
  it("follows Qt's order with the typed length", () => {
    const v = view({ session_status: "Could not refresh activity: timed out", session_status_error: true, status: "Could not refresh activity: timed out", status_error: true });
    expect(statusLine(v, undefined, false)).toEqual({ text: "Could not refresh activity: timed out", error: true });
    expect(statusLine(v, undefined, true)).toEqual({ text: TOO_LONG, error: true });
    expect(statusLine(v, "Could not read the pasted image.", true)).toEqual({ text: "Could not read the pasted image.", error: true });
    // Waiting outranks the composer's own error.
    const waiting = view({ waiting: "Waiting for the session to start.", status: "Waiting for the session to start." });
    expect(statusLine(waiting, "own", false)).toEqual({ text: "Waiting for the session to start.", error: false });
    // The draft's notice outranks the length.
    const notice = view({ uncertain: true, status: "Delivery is not confirmed." });
    expect(statusLine(notice, undefined, true)).toEqual({ text: "Delivery is not confirmed.", error: false });
    // Rust still saw text over 64 KiB; the typed text is shorter now.
    const stale = view({ status: TOO_LONG, status_error: true, unavailable_reason: "Session stopped. Send asks whether to resume it or start fresh." });
    expect(statusLine(stale, undefined, false)).toEqual({ text: "Session stopped. Send asks whether to resume it or start fresh.", error: false });
  });
});

describe("the batch envelope", () => {
  it("a composer view or a draft key set is content", () => {
    expect(isEmptyBatch(normalize({}))).toBe(true);
    expect(isEmptyBatch(normalize({ composer: view() }))).toBe(false);
    expect(isEmptyBatch(normalize({ draft_keys: [] }))).toBe(false);
    // Notices and Terminal requests that arrive after their call are content too (A2).
    const events = [{ kind: "notice" as const, text: "Response recorded.", error: false, transient: false }];
    expect(isEmptyBatch(normalize({ composer_events: events }))).toBe(false);
    expect(normalize({ composer_events: events }).composer_events).toEqual(events);
    expect(normalize({ draft_keys: [KEY] }).draft_keys).toEqual([KEY]);
  });

  describe("local rows (Task 7)", () => {
    const failed = { id: "req-1", text: "hello", status: "error" as const, attachments: [], run_id: "run-1", conversation_id: "conv-1",
      submitted_at: 1_793_000_000, error: "hgs: refused", uncertain: false, message_id: null, submitted_text: null, source: null, voice: null };

    it("Retry flushes the editor first and its cold question opens for the same composer", async () => {
      const f = fakes();
      f.retryWith({ kind: "cold", token: 7, prompt: { title: "Continue with a cold cache?", text: "", informative: "", choices: [] } });
      await createRoot(async () => {
        const c = createComposer(f.port, f.timers, images(1, 1), f.notify);
        c.apply(view({ local: [failed] }));
        c.input("typed", 5, 5);
        await c.localRetry("req-1");
        // A failed row flushes every key at once; the Retry finds nothing left to flush.
        expect(f.calls).toEqual(["edit 1 typed", "retry 1 req-1"]);
        expect(c.dialog()).toMatchObject({ kind: "cold", token: 7 });
        expect(c.locked()).toBe(true);
      });
    });

    it("a Retry reply that arrives after another composer opened keeps only its notice, and its question answers Cancel", async () => {
      const f = fakes();
      const retry = held<SendReply>();
      const port: ComposerPort = { ...f.port, localRetry: (h, id) => { f.calls.push(`retry ${h} ${id}`); return retry.call(); } };
      await createRoot(async () => {
        const c = createComposer(port, f.timers, images(1, 1), f.notify);
        c.apply(view({ local: [failed] }));
        const first = c.localRetry("req-1");
        await tick();
        c.apply(view({ handle: 2, key: "studio\nclaude/sample-project/other", text_epoch: 1 }));
        retry.answer({ kind: "cold", token: 9, prompt: { title: "Continue with a cold cache?", text: "", informative: "", choices: [] } });
        await first;
        await tick();
        expect(c.dialog()).toBeUndefined();
        expect(f.calls).toContain('answer 9 {"kind":"cancel"}');
        const second = c.localRetry("req-1");
        await tick();
        c.apply(view({ handle: 3, key: "studio\nclaude/sample-project/third", text_epoch: 1 }));
        retry.answer({ kind: "notice", text: "The session changed. Your draft was kept without sending.", error: true });
        await second;
        expect(f.notices).toEqual(["error sticky The session changed. Your draft was kept without sending."]);
      });
    });

    it("one Retry runs per row: a second click while it is on its way sends nothing", async () => {
      const f = fakes();
      const retry = held<SendReply>();
      const port: ComposerPort = { ...f.port, localRetry: (h, id) => { f.calls.push(`retry ${h} ${id}`); return retry.call(); } };
      await createRoot(async () => {
        const c = createComposer(port, f.timers, images(1, 1), f.notify);
        c.apply(view({ local: [failed] }));
        const first = c.localRetry("req-1");
        await tick();
        await c.localRetry("req-1");
        expect(f.calls.filter((x) => x.startsWith("retry"))).toEqual(["retry 1 req-1"]);
        retry.answer({ kind: "busy" });
        await first;
        // A refused Retry hands nothing back: the editor is not locked by it.
        expect(c.locked()).toBe(false);
        const again = c.localRetry("req-1");
        await tick();
        expect(f.calls.filter((x) => x.startsWith("retry"))).toEqual(["retry 1 req-1", "retry 1 req-1"]);
        retry.answer({ kind: "sending" });
        await again;
      });
    });

    it("Check answers where it leads only while its composer is still open", async () => {
      const f = fakes();
      let answer: (v: { kind: "terminal" } | null) => void = () => {};
      const port: ComposerPort = { ...f.port, localInspect: () => new Promise((resolve) => { answer = resolve; }) };
      await createRoot(async () => {
        const c = createComposer(port, f.timers, images(1, 1), f.notify);
        c.apply(view({ local: [{ ...failed, uncertain: true }] }));
        const shown = c.localInspect("req-1");
        answer({ kind: "terminal" });
        expect(await shown).toEqual({ kind: "terminal" });
        const late = c.localInspect("req-1");
        c.apply(view({ handle: 2, key: "studio\nclaude/sample-project/other" }));
        answer({ kind: "terminal" });
        expect(await late).toBeUndefined();
        await c.localDelete("req-1");
        expect(f.calls).toContain("delete 2 req-1");
      });
    });
  });
});

describe("suggestion dismissal", () => {
  it("answers valid field sends and pushes, never queue-only or disabled sends", async () => {
    const f = fakes(), dismissed: string[] = [];
    await createRoot(async () => {
      const c = createComposer(f.port, f.timers, undefined, f.notify, (key) => dismissed.push(key));
      await c.open(SESSION);
      c.input("draft", 5, 5);
      await c.send();
      expect(dismissed).toEqual([SESSION.key]);
      c.apply(view({ text: "next", text_epoch: 2 }));
      await c.push();
      expect(dismissed).toEqual([SESSION.key, SESSION.key]);
      c.apply(view({ text: "", text_epoch: 3, sending: true }));
      await c.push();
      expect(dismissed).toHaveLength(2);
      c.apply(view({ text: "blocked", text_epoch: 4, sending: false, available: false }));
      await c.send();
      expect(dismissed).toHaveLength(2);
    });
  });
});

describe("Clear session grace integration", () => {
  it("closes confirmation, permits a send during grace and Undo calls no Clear writer", async () => {
    const f = fakes(), gate = held<SendReply | undefined>();
    await createRoot(async (dispose) => {
      const c = createComposer(f.port, f.timers, undefined, f.notify, undefined, async (session, control) => {
        expect(session?.name).toBe(SESSION.name); expect(control.run).toBe("run-1"); return gate.call();
      });
      await c.open(SESSION); c.openClear(); const job = c.confirmClear();
      expect(c.dialog()).toBeUndefined(); expect(c.locked()).toBe(false);
      c.input("message during grace", 20, 20); await c.send();
      expect(f.calls.some((line) => line.includes("message during grace"))).toBe(true);
      c.apply(view({ text_epoch: 2 }));
      gate.answer(undefined); await job;
      expect(f.calls.some((line) => line.startsWith("clear "))).toBe(false);
      dispose();
    });
  });
});


describe("confirmed Clear target", () => {
  it("keeps the original clear after switching to another composer during Undo", async () => {
    const f = fakes(), gate = held<void>();
    await createRoot(async (dispose) => {
      const c = createComposer(f.port, f.timers, undefined, f.notify, undefined, async (...args) => {
        // Selection is not a target-validity condition for a confirmed action.
        expect(args).toHaveLength(3);
        await gate.call();
        return args[2]({ id: "confirmed-clear", index: 0 }, "clear");
      });
      await c.open(SESSION); c.openClear(); const job = c.confirmClear();
      c.apply(view({ handle: 2, key: "studio\nclaude/example/other", text: "other draft" }));
      gate.answer(); await job;
      expect(f.calls.filter((line) => line.startsWith("clear "))).toEqual(["clear run-1 conv-1 false"]);
      expect(c.text()).toBe("other draft");
      dispose();
    });
  });
});
