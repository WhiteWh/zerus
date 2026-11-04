import { createEffect, createSignal, onCleanup, onMount } from "solid-js";
import type { Terminal as XTerm } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import { Icon } from "../lib/icons";
import { contentScale } from "../../store/ui";
import { skin } from "../../skin/runtime";
import { screenShowsLatest, terminal, type Screen } from "../../store/terminal";
import { tr } from "../../i18n";

// xterm.js 6 loads when Terminal first opens. Linux keeps the DOM renderer;
// macOS and Windows add the WebGL addon and fall back to DOM when its
// context is lost. Scrollback is Qt's MaxHistoryLines (5000).

const encoder = new TextEncoder();
const webgl = typeof navigator !== "undefined" && /Mac|Windows/.test(navigator.userAgent);
const SCROLLBACK = 5000;

function theme() {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  return { background: v("--z-color-terminal-bg"), foreground: v("--z-color-terminal-fg"), cursor: v("--z-color-accent") };
}

export function TerminalTab() {
  let host!: HTMLDivElement;
  const [live, setLive] = createSignal<{ term: XTerm; measure: () => void } | undefined>();
  let gone = false;
  let teardown: (() => void) | undefined;
  // Registered synchronously: the owner is gone once the imports below resolve.
  onCleanup(() => { gone = true; teardown?.(); });

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
    terminal.paste(event.clipboardData?.getData("text/plain") ?? "");
  };

  onMount(async () => {
    host.addEventListener("paste", onPaste, true);
    onCleanup(() => host.removeEventListener("paste", onPaste, true));
    const [{ Terminal }, { FitAddon }] = await Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit"), import("@xterm/xterm/css/xterm.css")]);
    if (gone) return;
    const css = getComputedStyle(document.documentElement);
    const term = new Terminal({ fontFamily: css.getPropertyValue("--z-font-mono"), fontSize: Math.round(13 * contentScale()), cursorBlink: true, scrollback: SCROLLBACK, theme: theme() });
    const fit: FitAddon = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    if (webgl) {
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
    term.onData((data) => terminal.input(encoder.encode(data), screen));
    term.onBinary((data) => terminal.input(Uint8Array.from(data, (c) => c.charCodeAt(0) & 255), screen));
    term.onResize(({ cols, rows }) => terminal.resized(cols, rows));
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    teardown = () => { terminal.hide(); observer.disconnect(); term.dispose(); };
    setLive({ term, measure });
    terminal.show(screen);
    term.focus();
  });

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
        <span class="grow" />
        <button class="icon-btn" data-tip={reconnectTip()} aria-label={tr("Reconnect terminal")} disabled={!terminal.ready()} onClick={() => { void terminal.reconnect(); }}>
          <Icon name="refresh-cw" size={15} />
        </button>
        <button class="icon-btn" data-tip={toggleTip()} aria-label={connected() ? tr("Disconnect terminal") : tr("Connect terminal")} disabled={!connected() && !terminal.ready()}
          onClick={() => { void terminal.toggle(); }}>
          <Icon name={connected() ? "unplug" : "plug"} size={15} />
        </button>
      </div>
      <div class="term-host" data-shortcut-context="terminal" ref={host} />
    </div>
  );
}
