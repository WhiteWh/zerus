import { createSignal } from "solid-js";
import * as bridge from "../bridge";
import type { PageVisibility } from "./visibility";
import type { TerminalConnected, TerminalEvent, TerminalStatus, TerminalTarget } from "../types";

// The Terminal tab's state (Qt TerminalView): one attach per view, connected
// while the tab shows. Output is written to the screen and acknowledged once
// xterm parsed it, so Rust delivers at most two messages ahead. A hidden view
// acknowledges at once and drops the bytes: Rust keeps what it delivered and a
// view shown again redraws from it. Writes run one at a time, in order.
// A hidden or minimised window disconnects (Qt SessionsWindow::hideEvent calls
// disconnectSession); showing it again connects nothing (Qt's showEvent does
// not). "Hidden" is the app window's state
// reported by src-tauri (hide or minimize), not the webview's occlusion.
// connect, disconnect and retarget go to Rust through one chain, so it sees
// them in the order they were issued.

export const VIEW = "main";
/** At most one resize per this interval reaches Rust, plus the trailing one. */
export const RESIZE_MS = 100;

export interface TerminalPort {
  connect(view: string, target: TerminalTarget, cols: number, rows: number, onEvent: (message: TerminalEvent) => void): Promise<TerminalConnected>;
  write(view: string, generation: number, bytes: Uint8Array): Promise<void>;
  resize(view: string, generation: number, cols: number, rows: number): Promise<void>;
  ack(view: string, generation: number): void;
  disconnect(view: string): Promise<string | null>;
  retarget(view: string, target: TerminalTarget): Promise<boolean>;
  /** The tab of attach `generation` shows or hides (a dropped file's paste refuses while it hides). */
  shown(view: string, generation: number, shown: boolean): void;
}

export interface Screen {
  write(bytes: Uint8Array, done: () => void): void;
  /** xterm's own paste: bracketed when the program asked for it; the result arrives as input. */
  paste?(text: string): void;
  reset(): void;
  size(): { cols: number; rows: number };
  /** Scrolled to the bottom with text on the screen (Qt's checkViewedReply reads the screen's scroll bar and text). */
  latest?(): boolean;
}

/** What `screenShowsLatest` reads of xterm: the active buffer's scroll position and lines. */
export interface ScreenBuffer {
  rows: number;
  buffer: { active: { viewportY: number; baseY: number; getLine(y: number): { translateToString(trimRight?: boolean): string } | undefined } };
}

/**
 * Scrolled to the bottom and the screen is not blank (Qt's checkViewedReply:
 * the scroll bar at its maximum and the screen's text not empty). Reads
 * xterm's buffer only, no layout.
 */
export function screenShowsLatest(term: ScreenBuffer): boolean {
  const buffer = term.buffer.active;
  if (buffer.viewportY < buffer.baseY) return false;
  for (let y = 0; y < term.rows; y++) if (buffer.getLine(buffer.viewportY + y)?.translateToString(true).trim()) return true;
  return false;
}

export interface Timers { now(): number; later(fn: () => void, ms: number): () => void }
const realTimers: Timers = { now: () => performance.now(), later: (fn, ms) => { const id = setTimeout(fn, ms); return () => clearTimeout(id); } };

/**
 * Qt's TerminalScreen::pasteText filter: clipboard text is text, never terminal
 * commands (an embedded ESC [ 201 ~ must not end the bracketed paste). Keeps
 * tab, LF and CR; drops other C0 controls, DEL and C1; CRLF becomes LF.
 */
export function cleanPaste(text: string): string {
  let out = "";
  for (const ch of text) {
    const code = ch.codePointAt(0)!;
    if ((code >= 0x20 && code !== 0x7f && !(code >= 0x80 && code <= 0x9f)) || code === 9 || code === 10 || code === 13) out += ch;
  }
  return out.replace(/\r\n/g, "\n");
}

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));

export function createTerminal(port: TerminalPort, timers: Timers = realTimers, visibility: PageVisibility = bridge.windowVisibility) {
  const [phase, setPhase] = createSignal<"idle" | "connecting" | "connected">("idle");
  const [status, setStatus] = createSignal("");
  const [texts, setTexts] = createSignal<TerminalStatus | undefined>();
  let target: TerminalTarget | undefined;
  let screen: Screen | undefined;
  let generation = 0;
  /** Acknowledgements owed for output that arrived before the connect resolved. */
  let owed = 0;
  let ops: Promise<unknown> = Promise.resolve();
  const queue = <T,>(run: () => Promise<T>): Promise<T> => {
    const next = ops.then(run, run);
    ops = next.then(() => undefined, () => undefined);
    return next;
  };
  /**
   * Whether Rust may hold an attach for the view: set when a connect is issued,
   * cleared by a confirmed disconnect, a target change or a finish, unless a
   * newer connect was issued meanwhile. The view's phase alone is not enough:
   * leaveSession() idles it while the attach still runs.
   */
  let rustHolds = false;
  let issued = 0;
  /** Set when the Sessions page was left: coming back does not connect (Qt's page switch only disconnects). */
  let held = false;
  /** Bumps on every connect, disconnect and target change: older Channels are ignored. */
  let serial = 0;
  let writes: Promise<void> = Promise.resolve();
  let pendingSize: { cols: number; rows: number } | undefined;
  let lastResize = Number.NEGATIVE_INFINITY;
  let resizeTimer: (() => void) | undefined;

  const ready = () => !!target && !!texts()?.available;

  interface Restore { next: boolean; parsing: boolean; connected?: TerminalConnected; cols: number; rows: number }
  const restored = (mine: number, restore: Restore) => {
    if (mine !== serial || restore.parsing || !restore.connected) return;
    setPhase("connected");
    setStatus(restore.connected.status);
    // The view may have been resized while its history was being parsed.
    const now = screen?.size();
    if (now && (now.cols !== restore.cols || now.rows !== restore.rows)) resized(now.cols, now.rows);
  };

  const handler = (mine: number, restore: Restore) => (m: TerminalEvent) => {
    if (mine !== serial) return;
    if (m instanceof ArrayBuffer || ArrayBuffer.isView(m)) {
      const view = m as ArrayBufferView;
      const bytes = m instanceof ArrayBuffer ? new Uint8Array(m) : new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
      // xterm calls back in a later task: use the generation current by then.
      const replay = restore.next;
      restore.next = false;
      const ack = () => {
        if (mine !== serial) return;
        if (generation !== 0) port.ack(VIEW, generation); else owed++;
        if (replay) { restore.parsing = false; restored(mine, restore); }
      };
      // Cancel an unfinished historical escape sequence so a live suffix
      // cannot complete a replayed query after input becomes enabled.
      let output = bytes;
      if (replay) { output = new Uint8Array(bytes.length + 1); output.set(bytes); output[bytes.length] = 0x18; }
      if (screen) screen.write(output, ack);
      else ack();
      return;
    }
    if (m.kind === "replay") {
      restore.next = true;
      restore.parsing = true;
      setPhase("connecting");
      return;
    }
    if (m.kind === "finished") {
      serial++;
      generation = 0;
      rustHolds = false;
      setPhase("idle");
      setStatus(m.status);
    }
  };

  /** Connects, or rebinds the running attach after the view was mounted again. */
  async function connect() {
    if (!ready() || !screen || phase() === "connecting" || visibility.hidden()) return;
    const mine = ++serial;
    const rebinding = phase() === "connected";
    setPhase("connecting");
    if (!rebinding) setStatus(texts()!.connecting);
    screen.reset();
    owed = 0;
    const { cols, rows } = screen.size();
    const sent = target!;
    const ticket = ++issued;
    const restore: Restore = { next: false, parsing: false, cols, rows };
    rustHolds = true;
    try {
      const c = await queue(() => port.connect(VIEW, sent, cols, rows, handler(mine, restore)));
      if (mine !== serial) return;
      generation = c.generation;
      // xterm parses asynchronously. The connect IPC can resolve before its
      // replay callback; keeping Connecting rejects every historical reply.
      restore.connected = c;
      restored(mine, restore);
      for (; owed > 0; owed--) port.ack(VIEW, generation);

    } catch (error) {
      if (ticket === issued) rustHolds = false;
      if (mine !== serial) return;
      setPhase("idle");
      setStatus(message(error));
    }
  }

  /** Ends Rust's attach through the ordered chain; the flag clears only if no newer connect was issued. */
  function hangUp(): Promise<string | null> {
    const ticket = issued;
    return queue(() => port.disconnect(VIEW)).then(
      (text) => { if (ticket === issued) rustHolds = false; return text; },
      () => null,
    );
  }

  async function disconnect() {
    if (phase() === "idle" && !rustHolds) return;
    const mine = ++serial;
    generation = 0;
    owed = 0;
    setPhase("idle");
    const text = await hangUp();
    // A connect issued meanwhile owns the status now.
    if (mine === serial) setStatus(text ?? texts()?.disconnected ?? "");
  }

  /** Drops the view's state at once so nothing reaches the attach; Rust still holds it until the next retarget or disconnect (`rustHolds`). */
  function leaveSession() {
    serial++;
    generation = 0;
    owed = 0;
    setPhase("idle");
  }

  function resized(cols: number, rows: number) {
    pendingSize = { cols, rows };
    if (resizeTimer) return;
    const wait = Math.max(0, lastResize + RESIZE_MS - timers.now());
    resizeTimer = timers.later(() => {
      resizeTimer = undefined;
      if (!pendingSize || phase() !== "connected") return;
      const size = pendingSize;
      pendingSize = undefined;
      lastResize = timers.now();
      void port.resize(VIEW, generation, size.cols, size.rows).catch(() => {});
    }, wait);
  }

  /** Mirrors the core's `target_changed` (one endpoint per view, so the computer stands for it). */
  const changedTarget = (old: TerminalTarget | undefined, next: TerminalTarget) => {
    if (!old) return true;
    const sameRun = old.run_id !== "" && old.run_id === next.run_id;
    return old.computer !== next.computer
      || (old.name !== next.name && !sameRun)
      || (old.run_id !== "" && next.run_id !== "" && old.run_id !== next.run_id);
  };

  // Qt's hideEvent: a hidden or minimised window drops the attach. Showing it
  // again connects nothing (Qt's showEvent does not): the Terminal connects by
  // a tab change, a selection change with the tab current, or the user.
  visibility.onChange(() => {
    if (!visibility.hidden()) return;
    if (generation !== 0) port.shown(VIEW, generation, false);
    void disconnect();
  });

  return {
    phase, status, ready,
    connected: () => phase() === "connected",
    /** The tab shows a connected attach at its bottom and not blank: the person sees the latest output. */
    showsLatest: () => phase() === "connected" && !!screen?.latest?.(),
    /** The current attach's generation, 0 without one (Qt's connectionGeneration). */
    generation: () => generation,
    /** The selected session's target and texts (on every change of either). */
    async setTarget(next: TerminalTarget | undefined, nextTexts: TerminalStatus | undefined) {
      setTexts(nextTexts);
      if (!next || !nextTexts) { await disconnect(); target = undefined; setStatus(""); return; }
      // Input stops reaching the previous session before the await.
      const dropped = changedTarget(target, next) && phase() !== "idle";
      if (dropped) leaveSession();
      const ticket = issued;
      const changed = await queue(() => port.retarget(VIEW, next));
      target = next;
      if (changed) {
        if (ticket === issued) rustHolds = false;
        serial++;
        generation = 0;
        setPhase("idle");
        screen?.reset();
        setStatus(nextTexts.available ? nextTexts.idle : nextTexts.reason);
      }
      if (!nextTexts.available) {
        if (phase() !== "idle" || rustHolds) { serial++; generation = 0; setPhase("idle"); await hangUp(); }
        setStatus(nextTexts.reason);
        return;
      }
      if (screen && phase() === "idle" && (changed || dropped || rustHolds)) void connect();
    },
    /** The tab shows `s`: connect (Qt's currentChanged), or redraw a running attach. */
    show(s: Screen) {
      screen = s;
      if (phase() === "connected" && generation !== 0) port.shown(VIEW, generation, true);
      if (!held) void connect();
    },
    /** The user opened the Terminal tab: showing it connects (Qt's currentChanged). */
    opened() { held = false; },
    /** The Sessions page was left: disconnect, and coming back does not connect (Qt's page switch). */
    async leavePage() { held = true; await disconnect(); },
    leaveSession,
    texts,
    /** The tab hides; the attach keeps running. */
    hide() {
      screen = undefined;
      if (generation !== 0) port.shown(VIEW, generation, false);
    },
    connect, disconnect,
    async reconnect() { await disconnect(); await connect(); },
    async toggle() { if (phase() === "idle") await connect(); else await disconnect(); },
    input(bytes: Uint8Array, source?: Screen) {
      if ((source && source !== screen) || phase() !== "connected" || !bytes.length) return;
      const gen = generation;
      writes = writes.then(() => port.write(VIEW, gen, bytes)).catch((error) => { setStatus(message(error)); });
    },
    resized,
    /** A paste, cleaned as Qt does, through xterm's bracketed paste. */
    paste(text: string) {
      const clean = cleanPaste(text);
      if (phase() !== "connected" || !clean) return;
      screen?.paste?.(clean);
    },
  };
}

export type TerminalStore = ReturnType<typeof createTerminal>;

/** The app's Terminal, on the bridge. */
export const terminal = createTerminal({
  connect: bridge.terminalConnect, write: bridge.terminalWrite, resize: bridge.terminalResize,
  ack: bridge.terminalAck, disconnect: bridge.terminalDisconnect, retarget: bridge.terminalRetarget, shown: (view, generation, shown) => bridge.terminalShown(view, generation, shown),
});
