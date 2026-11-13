import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import type { Terminal as XTerm } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import { Icon } from "../lib/icons";
import { contentScale, selected, setMenu, toast } from "../../store/ui";
import { composer } from "../../store/composer";
import { clearInput, clearableText, composerTarget } from "../../store/clearInput";
import { copyVerbatim, pasteText } from "../../bridge";
import { platform } from "../../store/shortcuts";
import { normalizeTerminalImage, terminalMenu } from "./terminal-clipboard";
import { moveFocus, selectionRange, terminalKey, type Cell } from "./terminal-keys";
import { skin } from "../../skin/runtime";
import { screenShowsLatest, terminal, type Screen } from "../../store/terminal";
import { tr } from "../../i18n";
import { teams, sessionIs } from "../../store/teams";
import { mountTerminalNotices } from "./terminal-notices";
import { leadNoticeMode, setLeadNoticeMode } from "./Activity";
import { NOTICE_MODES, type NoticeMode } from "./leadNotices";

// xterm.js 6 loads when Terminal first opens. Linux keeps the DOM renderer;
// macOS and Windows add the WebGL addon and fall back to DOM when its
// context is lost. Scrollback is Qt's MaxHistoryLines (5000).

const encoder = new TextEncoder();
const webgl = typeof navigator !== "undefined" && /Mac|Windows/.test(navigator.userAgent);
const SCROLLBACK = 5000;

function imagePasteFailure(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  switch (message) {
    case "Pasting an image needs the desktop app.": return tr("Pasting an image needs the desktop app.");
    case "Pasting image files into Terminal is not supported on this computer.": return tr("Pasting image files into Terminal is not supported on this computer.");
    case "Terminal changed or image bytes changed before transfer; nothing was pasted.": return tr("Terminal changed or image bytes changed before transfer; nothing was pasted.");
    default: return message;
  }
}

function theme() {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  return { background: v("--z-color-terminal-bg"), foreground: v("--z-color-terminal-fg"), cursor: v("--z-color-accent") };
}

export function TerminalTab() {
  let host!: HTMLDivElement;
  const [live, setLive] = createSignal<{ term: XTerm; measure: () => void } | undefined>();
  const [folds, setFolds] = createSignal<ReturnType<typeof mountTerminalNotices> | undefined>();
  const [imagePasteError, setImagePasteError] = createSignal("");
  let inputScreen: Screen | undefined;
  let gone = false;
  let teardown: (() => void) | undefined;
  // Registered synchronously: the owner is gone once the imports below resolve.
  onCleanup(() => { gone = true; inputScreen = undefined; teardown?.(); });

  // Reactive inputs apply once the terminal exists.
  createEffect(() => {
    const l = live();
    if (!l) return;
    skin();
    queueMicrotask(() => { l.term.options.theme = theme(); });
  });
  createEffect(() => {
    const l = live();
    if (!l) return;
    l.term.options.fontSize = Math.round(13 * contentScale());
    l.measure();
  });

  // Qt's pasteText: the clipboard's text is cleaned by the store and goes
  // through xterm's own paste; xterm's default handler never sees the event.
  const onPaste = (event: ClipboardEvent) => {
    event.preventDefault();
    event.stopPropagation();
    try {
      const data = event.clipboardData;
      const lease = inputScreen && !gone ? terminal.captureImage(inputScreen) : undefined;
      const items = [...(data?.items ?? [])];
      const imageItems = items.filter((item) => item.kind === "file" && item.type.startsWith("image/"));
      const image = imageItems[0]?.getAsFile()
        ?? [...(data?.files ?? [])].find((file) => file.type.startsWith("image/"));
      if (image || imageItems.length || [...(data?.types ?? [])].some((type) => type.startsWith("image/"))) {
        setImagePasteError("");
        if (!image || !lease) setImagePasteError(tr("Could not read the pasted image."));
        else void normalizeTerminalImage(image).then((png) => lease.paste(png)).catch((error) => {
          if (!gone && lease.current()) setImagePasteError(imagePasteFailure(error));
        });
        return;
      }
      terminal.paste(data?.getData("text/plain") ?? "");
    } catch { setImagePasteError(tr("Could not read the pasted image.")); }
  };

  // Qt's copySelection and pasteClipboard: the selection exactly as shown,
  // and the clipboard's text through the same cleaned paste as a paste event.
  const copySelection = () => {
    const text = live()?.term.getSelection() ?? "";
    if (text) void copyVerbatim(text).catch((error) => toast(tr("Terminal"), tr("Could not copy: %1", String(error)), "error"));
  };
  // The clipboard is read off the UI thread: its text goes only to the attach
  // the menu opened on, never to a session the retained tab moved to since,
  // before the click or during the read (zerus-q4ayu).
  const pasteClipboard = (attach: number) => {
    if (!attach) return;
    // Only a delivered paste takes the focus: a refused one leaves it where the person moved it (zerus-mp3c9).
    void pasteText().then((text) => { if (terminal.paste(text, attach)) live()?.term.focus(); })
      .catch((error) => toast(tr("Terminal"), tr("Could not paste: %1", String(error)), "error"));
  };
  const onContextMenu = (event: MouseEvent) => {
    event.preventDefault();
    const term = live()?.term;
    if (!term) return;
    const attach = terminal.generation();
    const items = terminalMenu({ selection: term.hasSelection(), mouseApp: term.modes.mouseTrackingMode !== "none", shift: event.shiftKey, on: platform },
      copySelection, () => pasteClipboard(attach));
    if (items) setMenu({ x: event.clientX, y: event.clientY, items });
  };

  onMount(async () => {
    host.addEventListener("paste", onPaste, true);
    onCleanup(() => host.removeEventListener("paste", onPaste, true));
    const [{ Terminal }, { FitAddon }] = await Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit"), import("@xterm/xterm/css/xterm.css")]);
    if (gone) return;
    const css = getComputedStyle(document.documentElement);
    const row = selected();
    const foldMode = row && isLead(row) ? leadNoticeMode(row) : "full";
    const term = new Terminal({ fontFamily: css.getPropertyValue("--z-font-mono"), fontSize: Math.round(13 * contentScale()), cursorBlink: true, scrollback: SCROLLBACK, theme: theme(), allowProposedApi: true });
    const fit: FitAddon = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    if (webgl && foldMode === "full") {
      try {
        const { WebglAddon } = await import("@xterm/addon-webgl");
        const gl = new WebglAddon();
        gl.onContextLoss(() => gl.dispose());
        term.loadAddon(gl);
      } catch { /* the DOM renderer stays */ }
    }
    if (gone) { term.dispose(); return; }
    const measure = () => { if (host.clientWidth > 0 && host.clientHeight > 0) fit.fit(); };
    measure();
    const screen: Screen = {
      write: (bytes, done) => term.write(bytes, done), paste: (text) => term.paste(text), reset: () => term.reset(), size: () => ({ cols: term.cols, rows: term.rows }),
      latest: () => screenShowsLatest(term),
    };
    // Terminal's own keys (terminal-keys.ts): copy, a new line, keyboard
    // selection, select all and scrolling never reach the agent; paste keys
    // arrive as onPaste, and every other key goes to the agent as before.
    let anchor: Cell | undefined, focus: Cell | undefined, selecting = false;
    term.onSelectionChange(() => { if (!selecting) anchor = focus = undefined; });
    const extend = (move: Parameters<typeof moveFocus>[1]) => {
      const buffer = term.buffer.active;
      if (!anchor || !focus || !term.hasSelection()) anchor = focus = { x: buffer.cursorX, y: buffer.baseY + buffer.cursorY };
      focus = moveFocus(focus, move, term.cols, buffer.length, (y) => buffer.getLine(y)?.translateToString(true).length ?? 0);
      const range = selectionRange(anchor, focus, term.cols);
      selecting = true;
      try { if (range) term.select(range.column, range.row, range.length); else term.clearSelection(); } finally { selecting = false; }
      if (focus.y < buffer.viewportY) term.scrollToLine(focus.y);
      else if (focus.y >= buffer.viewportY + term.rows) term.scrollToLine(focus.y - term.rows + 1);
    };
    term.attachCustomKeyEventHandler((event) => {
      const action = terminalKey(event, platform, term.hasSelection());
      if (action.kind === "pass") return true;
      if (event.type !== "keydown") return false;
      event.preventDefault();
      if (action.kind === "copy") { copySelection(); if (action.clear) term.clearSelection(); }
      else if (action.kind === "newline") terminal.input(encoder.encode("\n"), screen);
      else if (action.kind === "select") extend(action.move);
      else if (action.kind === "selectAll") term.selectAll();
      else if (action.by === "page") term.scrollPages(action.up ? -1 : 1);
      else if (action.by === "line") term.scrollLines(action.up ? -1 : 1);
      else if (action.up) term.scrollToTop();
      else term.scrollToBottom();
      return false;
    });
    term.onData((data) => terminal.input(encoder.encode(data), screen));
    term.onBinary((data) => terminal.input(Uint8Array.from(data, (c) => c.charCodeAt(0) & 255), screen));
    term.onResize(({ cols, rows }) => terminal.resized(cols, rows));
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    inputScreen = screen;
    const fold = mountTerminalNotices(term, foldMode as NoticeMode);
    setFolds(fold);
    teardown = () => { inputScreen = undefined; fold.dispose(); setFolds(undefined); terminal.hide(); observer.disconnect(); term.dispose(); };
    setLive({ term, measure });
    terminal.show(screen);
    term.focus();
  });

  createEffect(() => {
    const fold = folds(), row = selected();
    if (fold) fold.setMode(row && isLead(row) ? leadNoticeMode(row) : "full");
  });
  const isLead = (row: ReturnType<typeof selected>): boolean => {
    if (!row) return false;
    const team = teams.teamOf(row);
    return !!team?.members.some((member) => member.lead && sessionIs(member.session, row.computer, row));
  };
  // zerus-wkj9l: Clear input, for text left in the agent's own composer. It
  // shows only for a recognized non-empty composer with nothing being sent,
  // and clears only after the person confirms the exact text.
  createEffect(() => {
    const s = selected();
    clearInput.follow(s?.key, composerTarget(s));
  });
  const reading = setInterval(() => { if (!document.hidden) void clearInput.refresh(); }, 2500);
  onCleanup(() => { clearInterval(reading); clearInput.follow(undefined); });
  const clearable = () => clearableText(clearInput.state(), selected(), composer.view(), clearInput.clearing());
  const [confirming, setConfirming] = createSignal<{ key: string; text: string; origin: HTMLElement }>();
  const confirmClear = async (key: string, text: string) => {
    setConfirming(undefined);
    const outcome = await clearInput.clear(key, text, clearable);
    if (!outcome) {
      toast(tr("Terminal"), tr("Input not cleared: %1", tr("The input changed or a message is being sent.")), "error");
      return;
    }
    if (outcome.status === "cleared") toast(tr("Terminal"), tr("Input cleared."));
    else if (outcome.status === "failed") toast(tr("Terminal"), tr("Input not cleared: %1", outcome.error), "error");
    else toast(tr("Terminal"), tr("Clearing the input is not confirmed: %1 Check Terminal.", outcome.error), "error", true);
  };

  const connected = () => terminal.connected();
  // Qt's updateButtons tooltips; unavailable shows the reason.
  const reason = () => terminal.texts()?.reason ?? "";
  const available = () => !!terminal.texts()?.available;
  const toggleTip = () => (connected() ? tr("Disconnect terminal. The session keeps running") : available() ? tr("Connect terminal") : reason());
  const reconnectTip = () => (available() ? tr("Reconnect terminal. The session keeps running") : reason());
  return (
    <div class="term-tab">
      <div class="term-bar">
        <span class="ell" title={terminal.status()}>{terminal.status()}</span>
        <Show when={selected() && isLead(selected())}>
          <label class="term-notice-mode" title={tr("How the team's notices to the lead and its replies to them show here.")}>
            <span class="muted">{tr("Team notices")}</span>
            <select class="field small" data-notice-mode value={leadNoticeMode(selected()!)} onChange={(event) => setLeadNoticeMode(selected()!, event.currentTarget.value as NoticeMode)}>
              <For each={NOTICE_MODES}>{(mode) => <option value={mode}>{mode === "full" ? tr("Full") : mode === "collapsed" ? tr("Collapsed") : tr("Hidden")}</option>}</For>
            </select>
          </label>
        </Show>
        <span class="grow" />
        <Show when={clearable()}>{(text) => (
          <button class="icon-btn term-clear-input" data-tip={tr("Clear input")} aria-label={tr("Clear input")}
            onClick={(event) => { const s = selected(); if (s) setConfirming({ key: s.key, text: text(), origin: event.currentTarget }); }}>
            <Icon name="eraser" size={15} />
          </button>
        )}</Show>
        <button class="icon-btn" data-tip={reconnectTip()} aria-label={tr("Reconnect terminal")} disabled={!terminal.ready()} onClick={() => { void terminal.reconnect(); }}>
          <Icon name="refresh-cw" size={15} />
        </button>
        <button class="icon-btn" data-tip={toggleTip()} aria-label={connected() ? tr("Disconnect terminal") : tr("Connect terminal")} disabled={!connected() && !terminal.ready()}
          onClick={() => { void terminal.toggle(); }}>
          <Icon name={connected() ? "unplug" : "plug"} size={15} />
        </button>
      </div>
      <Show when={imagePasteError()}>
        <div class="term-image-paste" role="status">
          <span>{imagePasteError()}</span>
          <button class="icon-btn small" aria-label={tr("Dismiss")} onClick={() => setImagePasteError("")}><Icon name="x" size={14} /></button>
        </div>
      </Show>
      <div class="term-host" data-shortcut-context="terminal" ref={host} onContextMenu={onContextMenu} />
      <Show when={confirming()}>{(item) => <ClearInputConfirm text={item().text} origin={item().origin}
        cancel={() => setConfirming(undefined)} confirm={() => void confirmClear(item().key, item().text)} />}</Show>
    </div>
  );
}

/** The exact text Clear input discards, confirmed before any key is sent. */
export function ClearInputConfirm(props: { text: string; origin: HTMLElement; cancel(): void; confirm(): void }) {
  // Read once: after the parent's <Show> unmounts this, its props are stale.
  const text = props.text, origin = props.origin;
  let dialog!: HTMLDialogElement, cancel!: HTMLButtonElement;
  onMount(() => { dialog.showModal(); cancel.focus(); });
  onCleanup(() => {
    if (dialog.open) dialog.close();
    queueMicrotask(() => { if (origin.isConnected) origin.focus(); });
  });
  return <dialog ref={dialog} class="dlg" style={{ width: "520px" }} aria-labelledby="clear-input-title"
    onCancel={(event) => { event.preventDefault(); props.cancel(); }} onClose={props.cancel}>
    <div class="dlg-head"><h2 id="clear-input-title">{tr("Clear input")}</h2></div>
    <div class="dlg-body"><div class="confirm-body">
      <p>{tr("Discard this text from the agent's input? It is not sent.")}</p>
      <pre class="clear-input-text">{text}</pre>
    </div></div>
    <div class="dlg-foot"><span class="spacer" />
      <button type="button" class="btn solid-danger" onClick={props.confirm}>{tr("Clear input")}</button>
      <button ref={cancel} type="button" class="btn primary" onClick={props.cancel}>{tr("Cancel")}</button>
    </div>
  </dialog>;
}
