import { bus as noticeBus } from "./notices";
import { batch, createMemo, createRoot, createSignal } from "solid-js";
import * as bridge from "../bridge";
import { notify } from "./actions";
import { suggestions } from "./suggestions";
import { documentVisibility, type PageVisibility } from "./visibility";
import type { Timers } from "./terminal";
import type { Answer, ClearControl, ColdPrompt, ComposerView, Inspect, SendReply, SessionRef, StartQuestion } from "../types";
import { trNoop, tr } from "../i18n";

// The open composer (Qt MessageComposer) over the Rust hub. The webview owns
// the text while the user types: edits go to Rust 250 ms after the last key,
// on blur and before any attach, paste, remove or send, each with a growing
// `rev` and the `text_epoch` the text is based on (Rust ignores an edit over
// older text). While the view has an error, an uncertain mark or a failed
// local row every key goes at once, since Rust may change the text itself
// then. Rust owns the attachments and the delivery state, and a growing
// `text_epoch` hands the text back (a load, an attach or remove, an Undo, a
// delivery's end, a refused change).
//
// The editor is read-only (Qt's `locked`, MessageComposer.cpp:1148-1149):
// while another session's composer opens; while an attach or paste runs (one
// at a time, as Qt's synchronous attach); during a remove or Undo until a
// view with a higher epoch arrives; while a send or an answer is on its way
// and until Rust's view takes the lock over (sending, waiting) or hands the
// text back; while the cold cache or start question is open (it keeps the
// epoch, so closing it unlocks); while Rust says the draft is not loaded, is
// sending, attaching or waiting; and while the session status blocks input.
//
// Every call keeps the handle it started on. A reply that arrives after
// another composer opened changes nothing there: its notice still shows, a
// question it asks is answered Cancel, and the rest is dropped.

export const EDIT_MS = 250;
export const MAX_TEXT_BYTES = 64 * 1024;
/** Qt refuses a pasted image above this many pixels (MessageComposer.cpp:394; core `MAX_IMAGE_PIXELS`). */
export const MAX_PIXELS = 32_000_000;
export const TOO_LONG = trNoop("Message exceeds 64 KiB.");
const TOO_LARGE = trNoop("Image is too large. Use an image below 32 megapixels.");
const UNREADABLE = trNoop("Could not read the pasted image.");
/** How long the disabled Send's hint shows (Qt's QToolTip, MessageComposer.cpp:817). */
const HINT_MS = 5000;

export interface ComposerPort {
  open(session: SessionRef | undefined): Promise<ComposerView | null>;
  edit(handle: number, rev: number, epoch: number, text: string, position: number, anchor: number): Promise<void>;
  pick(handle: number, start: number, end: number): Promise<void>;
  /** Attaches files by path, their references going where `start..end` is (composer_attach). */
  attach(handle: number, paths: string[], start: number, end: number): Promise<void>;
  pasteImage(handle: number, png: Uint8Array, start: number, end: number): Promise<void>;
  send(handle: number, rev: number, epoch: number, text: string, position: number, anchor: number): Promise<SendReply>;
  push(handle: number, rev: number, epoch: number, text: string, position: number, anchor: number): Promise<SendReply>;
  sendNow(queueId: string | null): Promise<boolean>;
  answer(token: number, answer: Answer): Promise<SendReply>;
  remove(handle: number, index: number): Promise<void>;
  undo(handle: number): Promise<void>;
  allowRetry(handle: number): Promise<void>;
  cancelSend(handle: number): Promise<void>;
  interrupted(handle: number): Promise<void>;
  clear(handle: number, run: string, conversation: string, dismiss: boolean, delay?: import("./endActionBusy").EndReference): Promise<SendReply>;
  refresh(handle: number): Promise<void>;
  /** Opens a draft attachment by its `zerus-file` path: null when it opened, else a notice; rejects with why not (Task 9). */
  openAttachment(path: string): Promise<string | null>;
  /** A local row's Retry, Delete and Check (src-tauri `local_retry`, `local_delete`, `local_inspect`). */
  localRetry(handle: number, id: string): Promise<SendReply>;
  localDelete(handle: number, id: string): Promise<void>;
  localInspect(handle: number, id: string): Promise<Inspect | null>;
}

/** Image decoding for pastes that are not PNG; tests pass their own. */
export interface Images {
  size(blob: Blob): Promise<{ width: number; height: number }>;
  png(blob: Blob): Promise<Uint8Array>;
}

/** Window notices (store/actions.ts `notify`); tests pass their own. */
export type Notices = (tone: "error" | "info", text: string, sticky: boolean) => void;

const browserImages: Images = {
  async size(blob) {
    const bitmap = await createImageBitmap(blob);
    const size = { width: bitmap.width, height: bitmap.height };
    bitmap.close();
    return size;
  },
  async png(blob) {
    const bitmap = await createImageBitmap(blob);
    const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
    canvas.getContext("2d")!.drawImage(bitmap, 0, 0);
    bitmap.close();
    return new Uint8Array(await (await canvas.convertToBlob({ type: "image/png" })).arrayBuffer());
  },
};

const realTimers: Timers = { now: () => performance.now(), later: (fn, ms) => { const id = setTimeout(fn, ms); return () => clearTimeout(id); } };
const appNotices: Notices = (tone, text, sticky) => { notify(tone, text, sticky); };
const utf8 = new TextEncoder();

/** Qt's main/archive and four-part child draft keys (src-tauri `session_draft_key`). */
export const draftKey = (s: SessionRef) => s.subagent
  ? `${s.computer}\n${s.name}\n${s.subagent.parent_conversation_id}\n${s.subagent.agent_id}`
  : s.archive_id ? `${s.computer}\n${s.name}\n${s.archive_id}` : `${s.computer}\n${s.name}`;

export type Dialog =
  | { kind: "cold"; token: number; prompt: ColdPrompt }
  | { kind: "start"; token: number; question: StartQuestion }
  /** The Clear session dialog keeps the control it showed; the confirm sends its identity back. */
  | { kind: "clear"; control: ClearControl };

/** The status line (Qt's updateControls 1200-1204): text and whether it is in the error colour. */
export interface StatusLine { text: string; error: boolean }

/**
 * The status line from Rust's view, the composer's own error and the length
 * of the text being typed. Rust orders storage error, waiting, the draft's
 * notice and the attachment reason above the length; the session status and
 * the block reason come after it.
 */
export function statusLine(v: ComposerView | undefined, own: string | undefined, tooLong: boolean): StatusLine {
  if (v?.waiting) return { text: v.status, error: v.status_error };
  if (own) return { text: own, error: true };
  if (!v) return tooLong ? { text: tr(TOO_LONG), error: true } : { text: "", error: false };
  const below = v.status === "" || v.status === TOO_LONG || v.status === v.session_status || v.status === v.unavailable_reason;
  if (!below) return { text: v.status, error: v.status_error };
  if (tooLong) return { text: tr(TOO_LONG), error: true };
  // Rust's length check may still see older text: the next item then.
  const text = v.status === TOO_LONG ? v.session_status || v.unavailable_reason : v.status;
  return { text, error: text !== "" && text === v.session_status && v.session_status_error };
}

/**
 * The composers with a call of one kind under way, as a signal: each call
 * locks only its own composer, also after the user went to another one and
 * back while it runs.
 */
function handleSet() {
  const [handles, setHandles] = createSignal<ReadonlySet<number>>(new Set(), { equals: false });
  return {
    has: (h: number) => handles().has(h),
    add: (h: number) => setHandles((all) => new Set(all).add(h)),
    remove: (h: number) => setHandles((all) => { const next = new Set(all); next.delete(h); return next; }),
  };
}

export type ClearDriver = (session: SessionRef | undefined, control: ClearControl, execute: (reference: import("./endActionBusy").EndReference, key: string) => Promise<SendReply>) => Promise<SendReply | undefined>;
export function createComposer(port: ComposerPort, timers: Timers = realTimers, images: Images = browserImages, notices: Notices = appNotices, sent: (key: string) => void = () => {}, delayClear?: ClearDriver) {
  const [view, setView] = createSignal<ComposerView | undefined>();
  const [text, setText] = createSignal("");
  /** `start` is the anchor and `end` the cursor position (Qt's anchor and position); start may follow end. */
  const [selection, setSelection] = createSignal({ start: 0, end: 0 });
  /** The composers whose attach or paste runs. */
  const pending = handleSet();
  /** The composer's own error (a refused paste), Qt's `showError`: it stays until a hand-back, a send or Allow retry. */
  const [error, setError] = createSignal<string | undefined>();
  const [dialog, setDialog] = createSignal<Dialog | undefined>();
  /** The epoch a remove, Undo or refused send waits to see exceeded. */
  const [handBack, setHandBack] = createSignal<number | undefined>();
  /** The composers whose send or answer call is on its way. */
  const calling = handleSet();
  /** The local rows whose Retry is on its way, by composer and row: one click sends one Retry. */
  const retrying = new Set<string>();
  /** Opens under way: the editor stays read-only, so no key is typed into the composer being left. */
  const [opening, setOpening] = createSignal(0);
  /** Quit has asked for the last text: freeze typing while its bounded handshake runs. */
  const [closing, setClosing] = createSignal(false);
  /** Edits already handed to IPC, including a composer left meanwhile. */
  const edits = new Set<Promise<void>>();
  /** A send started: locked until Rust's view shows it (sending, waiting) or hands the text back. */
  const [starting, setStarting] = createSignal(false);
  /** The disabled Send's hint (Qt shows the status as a tooltip at the editor). */
  const [hint, setHint] = createSignal<string | undefined>();
  let rev = 0;
  let epoch = 0;
  let dirty = false;
  let timer: (() => void) | undefined;
  let hintTimer: (() => void) | undefined;
  /** The draft key of the session last opened; a view of another composer is an older answer. */
  let expected: string | undefined;
  let openedSession: SessionRef | undefined;
  let suggestionKey = "";
  let opens = 0;
  let inFlight = 0;
  /** The newest envelope view while an open runs: it may be older than the open's answer. */
  let held: ComposerView | undefined;

  const handle = () => view()?.handle;
  /** Over 64 KiB of UTF-8, measured once per text: a UTF-16 unit is 1 to 3 bytes, so most texts need no encoding. */
  const tooLong = createMemo(() => {
    const t = text();
    if (t.length * 3 <= MAX_TEXT_BYTES) return false;
    if (t.length > MAX_TEXT_BYTES) return true;
    return utf8.encode(t).length > MAX_TEXT_BYTES;
  });
  const question = () => { const d = dialog(); return d?.kind === "cold" || d?.kind === "start"; };
  /** Qt's `operationLocked`: everything but the session status's lock. */
  const operationLocked = () => {
    const v = view();
    const h = v?.handle;
    return closing() || opening() > 0 || (h !== undefined && (pending.has(h) || calling.has(h))) || handBack() !== undefined || starting() || question()
      || !v?.loaded || v.attaching || v.sending || !!v.recording || !!v.waiting;
  };
  const locked = () => operationLocked() || !!view()?.session_blocks_input;
  const ordered = () => { const s = selection(); return { start: Math.min(s.start, s.end), end: Math.max(s.start, s.end) }; };
  /** Rust may change the text itself: an error, an uncertain mark or a failed local row. */
  const immediate = () => { const v = view(); return !!v && (v.status_error || v.uncertain || v.local.some((row) => row.status === "error")); };
  const hasContent = () => text().trim() !== "" || (view()?.attachments.length ?? 0) > 0;
  /** Qt's Send state (MessageComposer.cpp:1185), from Rust's view and the text being typed. */
  const sendEnabled = () => {
    const v = view();
    // A required question card hides Send with the editor (setInputVisible).
    if (!v || !v.loaded || !v.available || v.uncertain || v.input_visible === false || locked() || tooLong() || !hasContent()) return false;
    return !(v.attachments.length > 0 && !v.attachments_available);
  };
  /** Qt adds the push action to this message field only (MessageComposer.cpp:841-853). */
  const pushEnabled = () => !closing() && opening() === 0 && !!view() && !view()?.subagent && view()?.input_visible !== false;
  /** The old session's queue must not remain actionable while its successor opens. */
  const queue = () => opening() > 0 ? null : view()?.queue ?? null;

  /** The composer goes or changes: its open question answers Cancel, as its modal dialog would. */
  function dropDialog() {
    const d = dialog();
    setDialog(undefined);
    if (d && d.kind !== "clear") void port.answer(d.token, { kind: "cancel" }).catch(() => {});
  }

  function apply(next: ComposerView | undefined) {
    batch(() => {
      if (!next) {
        timer?.();
        timer = undefined;
        dirty = false;
        setView(undefined);
        setHandBack(undefined);
        setStarting(false);
        setError(undefined);
        dropDialog();
        return;
      }
      if (next.handle !== view()?.handle) {
        epoch = 0;
        rev = next.rev;
        dirty = false;
        timer?.();
        timer = undefined;
        setHandBack(undefined);
        setStarting(false);
        setError(undefined);
        dropDialog();
      }
      setView(next);
      if (next.text_epoch > epoch) {
        epoch = next.text_epoch;
        setHandBack(undefined);
        setStarting(false);
        rev = Math.max(rev, next.rev);
        dirty = false;
        setText(next.text);
        setSelection({ start: next.anchor, end: next.position });
        setError(undefined);
      }
      if (next.sending || next.waiting) setStarting(false);
    });
  }

  /** A view from the batch envelope: only the composer last opened, and after any open under way. */
  function receive(next: ComposerView) {
    if (inFlight > 0) { held = next; return; }
    if (expected === undefined || (next.key !== expected && next.handle !== handle())) return;
    apply(next);
  }

  async function flush() {
    timer?.();
    timer = undefined;
    const h = handle();
    if (!dirty || h === undefined) return;
    dirty = false;
    const s = selection();
    let write: Promise<void> | undefined;
    try {
      write = port.edit(h, rev, epoch, text(), s.end, s.start);
      edits.add(write);
      await write;
    } catch { /* outside the app: nothing to keep */ }
    finally { if (write) edits.delete(write); }
  }

  /** The page acknowledges quit only after its last edit has reached Rust. */
  async function flushForExit() {
    setClosing(true);
    await flush();
    await Promise.allSettled([...edits]);
  }

  function changed() {
    rev++;
    dirty = true;
    timer?.();
    timer = undefined;
    if (immediate()) void flush();
    else timer = timers.later(() => { void flush(); }, EDIT_MS);
  }

  function showHint(textToShow: string) {
    hintTimer?.();
    setHint(textToShow);
    hintTimer = timers.later(() => { hintTimer = undefined; setHint(undefined); }, HINT_MS);
  }

  /** Opens the Clear session dialog with the control shown now or an unselected row's inspected control. */
  function openClear(inspected?: ClearControl) {
    const control = inspected ?? view()?.clear;
    if (control?.enabled && !dialog()) setDialog({ kind: "clear", control: { ...control } });
  }

  /**
   * A reply of a send, an answer or a clear: `h` is the composer its call
   * started on, `at` the epoch it was made at, `refused` whether a refusal
   * hands the text back. Another composer open by now keeps only the notice;
   * a question for the one left is answered Cancel.
   */
  function reply(r: SendReply, h: number, at: number, refused: boolean) {
    if (h !== handle()) {
      if (r.kind === "notice") notices(r.error ? "error" : "info", r.text, r.error);
      else if (r.kind === "cold" || r.kind === "start") void port.answer(r.token, { kind: "cancel" }).catch(() => {});
      return;
    }
    const v = view();
    switch (r.kind) {
      case "busy": case "blocked":
        // A refused Send hands its text back under a new epoch; an answer's refusal changes nothing.
        if (refused && epoch === at) setHandBack(at);
        break;
      case "sending":
        if (epoch === at && !v?.sending && !v?.waiting) setStarting(true);
        break;
      case "cold": setDialog({ kind: "cold", token: r.token, prompt: r.prompt }); break;
      case "start": setDialog({ kind: "start", token: r.token, question: r.question }); break;
      case "clear_context": openClear(); break;
      // Qt's showNotice(text, error): an error stays until dismissed, a plain notice goes (SessionsWindow.cpp:5210).
      case "notice": notices(r.error ? "error" : "info", r.text, r.error); break;
    }
  }

  /** Runs one attach or paste; a second while one runs does nothing. */
  async function exclusive(work: (h: number) => Promise<void>) {
    const h = handle();
    if (h === undefined || locked()) return;
    pending.add(h);
    try { await flush(); await work(h); } catch { /* outside the app */ } finally { pending.remove(h); }
  }

  /** Remove and Undo change the text in Rust: locked until it hands the text back. */
  async function handBackAfter(call: (h: number) => Promise<void>) {
    const h = handle();
    if (h === undefined || locked()) return;
    await flush();
    const at = epoch;
    setHandBack(at);
    try { await call(h); } catch { if (epoch === at) setHandBack(undefined); }
  }

  /** Panel and window shortcut use this hub command; a push keeps its own handle. */
  async function sendNow(queueId?: string): Promise<boolean> {
    if (closing() || view()?.subagent) return false;
    if (opening() > 0) {
      notices("info", tr("Nothing queued can be sent now"), false);
      return false;
    }
    try { return await port.sendNow(queueId ?? null); }
    catch (e) { notices("error", typeof e === "string" ? e : e instanceof Error ? e.message : String(e), true); return false; }
  }

  /** Ordinary Send and push share the flush, handle guard and reply hand-back. */
  async function sendField(pushing: boolean) {
    const h = handle();
    // Under a required question card the editor is hidden and takes no command (setInputVisible).
    if (h === undefined || calling.has(h) || view()?.input_visible === false || (pushing && !pushEnabled())) return;
    // Rust decides whether an empty/sending push becomes Send now, under
    // the captured handle (MessageComposer.cpp:837 carries the field's key).
    const queuePush = pushing && (view()?.sending || !hasContent());
    if (locked() && !queuePush) return;
    if (!queuePush && !sendEnabled()) {
      // Qt shows the status at the editor when there is something to send (MessageComposer.cpp:814-818).
      const status = statusLine(view(), error(), tooLong()).text;
      if (hasContent() && status) showHint(status);
      return;
    }
    hintTimer?.();
    setHint(undefined);
    // Locked before the flush awaits, so a second Enter meanwhile sends nothing.
    calling.add(h);
    let r: SendReply;
    let at = epoch;
    const captured = { rev, epoch, text: text(), position: selection().end, anchor: selection().start };
    try {
      await flush();
      if (h !== handle() || opening() > 0) { calling.remove(h); return; }
      if (!queuePush) sent(suggestionKey);
      setError(undefined);
      if (pushing) {
        r = await port.push(h, captured.rev, captured.epoch, captured.text, captured.position, captured.anchor);
      } else {
        at = epoch;
        const s = selection();
        r = await port.send(h, rev, at, text(), s.end, s.start);
      }
    } catch (e) {
      calling.remove(h);
      notices("error", typeof e === "string" ? e : e instanceof Error ? e.message : String(e), true);
      return;
    }
    batch(() => { calling.remove(h); reply(r, h, at, true); });
  }

  return {
    view, text, selection, dialog, locked, operationLocked, sendEnabled, pushEnabled, queue, hint,
    /** The status line: Qt's order, with the composer's own error and the typed length. */
    status: (): StatusLine => statusLine(view(), error(), tooLong()),
    apply, receive,
    async open(session: SessionRef | undefined) {
      if (closing()) return;
      openedSession = session ? { ...session } : undefined;
      setOpening((n) => n + 1);
      try {
        // The composer being left keeps what was typed: its edit goes first.
        await flush();
        suggestionKey = session?.key ?? "";
        expected = session ? draftKey(session) : undefined;
        const mine = ++opens;
        inFlight++;
        let next: ComposerView | null = null;
        try { next = await port.open(session); } catch { next = null; } finally { inFlight--; }
        if (mine !== opens) return;
        apply(next ?? undefined);
        const later = held;
        held = undefined;
        if (later && inFlight === 0) receive(later);
      } finally {
        setOpening((n) => n - 1);
      }
    },
    input(next: string, start: number, end: number) {
      if (locked() || !view()) return;
      batch(() => { setText(next); setSelection({ start, end }); });
      changed();
    },
    /** The cursor moved: Qt saves the draft with its cursor (rememberCursor). */
    select(start: number, end: number) {
      const s = selection();
      if (s.start === start && s.end === end) return;
      setSelection({ start, end });
      if (!locked() && view()) changed();
    },
    flush, flushForExit,
    attachFiles: () => exclusive((h) => { const s = ordered(); return port.pick(h, s.start, s.end); }),
    /** Qt's canAttachFiles: the session takes attachments and nothing locks the composer. */
    canAttach: () => !!view()?.attachments_available && !locked(),
    /**
     * Dropped files (MessageComposer::attachDroppedFiles, 961-967): the
     * selection collapses to the cursor and their references go there.
     */
    attachDropped: (paths: string[]) => exclusive((h) => { const at = selection().end; return port.attach(h, paths, at, at); }),
    async pasteImage(blob: Blob) {
      await exclusive(async (h) => {
        let png: Uint8Array;
        try {
          if (blob.type === "image/png") png = new Uint8Array(await blob.arrayBuffer());
          else {
            const { width, height } = await images.size(blob);
            if (width * height > MAX_PIXELS) { setError(tr(TOO_LARGE)); return; }
            png = await images.png(blob);
          }
        } catch { setError(tr(UNREADABLE)); return; }
        const s = ordered();
        await port.pasteImage(h, png, s.start, s.end);
      });
    },
    remove: (index: number) => handBackAfter((h) => port.remove(h, index)),
    undo: () => handBackAfter((h) => port.undo(h)),
    send: () => sendField(false),
    push: () => sendField(true),
    sendNow,
    /** Answers the open question with its token. The question kept the epoch: closing it unlocks the editor. */
    async answer(answer: Answer) {
      const d = dialog();
      const h = handle();
      if (!d || d.kind === "clear" || h === undefined) return;
      batch(() => { setDialog(undefined); calling.add(h); });
      let r: SendReply;
      let at = epoch;
      try {
        await flush();
        at = epoch;
        r = await port.answer(d.token, answer);
      } catch {
        calling.remove(h);
        return;
      }
      batch(() => { calling.remove(h); reply(r, h, at, false); });
    },
    /** Cancel or Escape: a question answers Cancel; the Clear dialog just closes. */
    closeDialog: dropDialog,
    openClear,
    /** The Clear session confirmation: the dialog closes first, since the call answers only once the clear ended (up to 45 s). */
    async confirmClear() {
      const d = dialog();
      setDialog(undefined);
      const h = handle();
      if (h === undefined || d?.kind !== "clear") return;
      const at = epoch;
      let r: SendReply;
      try {
        const execute = (delay?: import("./endActionBusy").EndReference) => port.clear(h, d.control.run, d.control.conversation, d.control.dismiss, delay);
        const result = delayClear ? await delayClear(openedSession, d.control, (ref) => execute(ref)) : await execute();
        if (!result) return;
        r = result;
      } catch (e) {
        notices("error", typeof e === "string" ? e : String(e), true);
        return;
      }
      reply(r, h, at, false);
    },
    async cancelSend() { const h = handle(); if (h !== undefined) await port.cancelSend(h).catch(() => {}); },
    async allowRetry() {
      const h = handle();
      if (h === undefined) return;
      setError(undefined);
      await port.allowRetry(h).catch(() => {});
    },
    /** Before a Stop interrupts: Rust cancels this composer's start or compaction (Qt 1187-1191). */
    async interrupted() { const h = handle(); if (h !== undefined) await port.interrupted(h).catch(() => {}); },
    /** The status line's Refresh (Qt's sessionRefreshRequested). */
    async refresh() { const h = handle(); if (h !== undefined && !operationLocked()) await port.refresh(h).catch(() => {}); },
    /**
     * Opens attachment `index` (MessageComposer::openAttachment, 1067-1085):
     * nothing while the draft sends. A program shown in the file manager, or
     * an open that failed, shows in the status line, as Qt's showError, while
     * this composer is still the open one.
     */
    async openAttachment(index: number) {
      const v = view(), a = v?.attachments[index];
      if (!v || !a || v.sending) return;
      let notice: string | null;
      try { notice = await port.openAttachment(a.path); } catch (e) { notice = typeof e === "string" ? e : String(e); }
      if (notice && handle() === v.handle) setError(notice);
    },
    /**
     * Retry on a failed local row (Qt's messageAction "retry"): Rust sends a
     * definite failure again as a new submission with a new request id, and
     * never an uncertain one. Its reply keeps the composer it started on, as
     * a send's: a cold question for a composer left meanwhile is answered
     * Cancel, and only a notice still shows.
     */
    async localRetry(id: string) {
      const h = handle();
      if (h === undefined) return;
      const mark = `${h}\n${id}`;
      if (retrying.has(mark)) return;
      retrying.add(mark);
      let r: SendReply;
      let at = epoch;
      try {
        await flush();
        at = epoch;
        r = await port.localRetry(h, id);
      } catch (e) {
        notices("error", typeof e === "string" ? e : e instanceof Error ? e.message : String(e), true);
        return;
      } finally {
        retrying.delete(mark);
      }
      reply(r, h, at, false);
    },
    /** Delete on a failed local row (the view asked first). */
    async localDelete(id: string) { const h = handle(); if (h !== undefined) await port.localDelete(h, id).catch(() => {}); },
    /** Check Terminal or Check native UI on a failed row: where it leads, or nothing once another composer opened. */
    async localInspect(id: string): Promise<Inspect | undefined> {
      const h = handle();
      if (h === undefined) return undefined;
      let routed: Inspect | null = null;
      try { routed = await port.localInspect(h, id); } catch { return undefined; }
      return routed && h === handle() ? routed : undefined;
    },
  };
}

export type ComposerStore = ReturnType<typeof createComposer>;

export const composer = createRoot(() => createComposer({
  open: bridge.composerOpen, edit: bridge.composerEdit, pick: bridge.composerPick, attach: (h, paths, start, end) => bridge.composerAttach(h, paths, start, end), pasteImage: bridge.composerPasteImage,
  send: bridge.sendMessage, push: (...args) => bridge.sendPush(...args), sendNow: (id) => bridge.queueSendNow(id), answer: bridge.sendAnswer, remove: bridge.composerRemove, undo: bridge.composerUndoRemove,
  allowRetry: bridge.composerAllowRetry, cancelSend: bridge.sendCancel, interrupted: bridge.sendInterrupted, clear: bridge.clearContext,
  refresh: bridge.composerRefresh, openAttachment: (path) => bridge.attachmentOpenDraft(path), localRetry: bridge.localRetry, localDelete: bridge.localDelete, localInspect: bridge.localInspect,
}, realTimers, browserImages, appNotices, suggestions.dismiss, async (session, control, execute) => {
  const [{ endActions }, { catalog, rowId }, { captureEnd, sameEnd }] = await Promise.all([import("./endActions"), import("./catalog"), import("./endActionBusy")]);
  const row = session ? catalog.rows[rowId(session.computer, session.key)] : undefined;
  if (!row || row.runId !== control.run || row.conversationId !== control.conversation) return;
  const captured = { ...row };
  return endActions.schedule([captureEnd(captured, { kind: "clear_context", dismiss_prompt: control.dismiss })], async ([ref], key) => {
    const reply = await execute(ref, key);
    if (reply.kind !== "notice") return reply;
    const changed = reply.error && reply.text === "The session state changed. Open its actions again.";
    noticeBus.notify(changed || !reply.error ? "info" : "error", changed ? tr("Clear cancelled: the session changed.") : reply.text, !changed && reply.error, key);
  }, { current: () => sameEnd(captured, catalog.rows[captured.key]) });
}));

/**
 * Typing not yet flushed goes to Rust when the window hides or the page goes:
 * `visibilities` report hiding (the page's and Rust's window-visibility
 * event), `page` fires pagehide.
 */
export function flushWhenHidden(flush: () => void, visibilities: PageVisibility[], page?: { addEventListener(type: "pagehide", listener: () => void): void }) {
  for (const v of visibilities) v.onChange(() => { if (v.hidden()) flush(); });
  page?.addEventListener("pagehide", () => flush());
}

flushWhenHidden(() => { void composer.flush(); }, [documentVisibility, bridge.windowVisibility], typeof window === "undefined" ? undefined : window);
