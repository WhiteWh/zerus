import { For, Show, createEffect, createSignal, on, onCleanup } from "solid-js";
import { tr } from "../../i18n";
import { Portal } from "solid-js/web";
import { Icon } from "../lib/icons";
import { archive, buttonText } from "../../store/archive";
import {
  MAX_DAY, MIN_DAY, PERIODS, applyDraft, draftHint, draftReversed, draftValid, editDraft, panelPlace, pickerLabel, rangeCaption, stageDraft, tabTarget, type Draft, type Period,
} from "./archive-dates";

// The Archive bar above the list under the Archive filter (Qt's
// ArchiveBrowser): the date range and Refresh, the status with Newer and
// Older, then the errors by machine.

const [open, setOpen] = createSignal(false);
/**
 * The date panel is open: a popup, so a hover-opened strip peek stays open
 * under it (Qt's activePopupWidget, SessionPanelDock.h 50-56).
 */
export const datePanelOpen = open;

export function ArchiveBar(props: { contentSearch?: boolean } = {}) {
  const [width, setWidth] = createSignal(0);
  const observe = (el: HTMLElement) => {
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    onCleanup(() => ro.disconnect());
  };
  const status = () => archive.status();
  // A new window starts at the top of the list (Qt's windowChanged).
  createEffect(on(archive.window, () => document.querySelector(".sp-panel .sp-scroll")?.scrollTo({ top: 0 }), { defer: true }));
  return (
    <div class="archive-browser" ref={observe}>
      <div class="archive-bar">
        <DatePicker />
        <button class="icon-btn archive-refresh" title={tr("Refresh archive")} aria-label={tr("Refresh archive")} disabled={!status().refreshEnabled} onClick={() => archive.refresh()}>
          <Icon name="refresh" size={16} />
        </button>
      </div>
      <Show when={!props.contentSearch}><div class="archive-bar">
        <span class="archive-status ell" title={status().hint} aria-description={status().hint}>{status().summary}</span>
        <Show when={status().newer.visible}>
          <button class="btn small archive-nav" title={tr("Newer sessions")} aria-label={tr("Newer sessions")} disabled={!status().newer.enabled} onClick={() => archive.newer()}>
            <Icon name="chevron-left" size={14} />{buttonText(width(), "Newer") ? tr("Newer") : ""}
          </button>
        </Show>
        <Show when={status().older.visible}>
          <button class="btn small archive-nav" title={tr("Older sessions")} aria-label={tr("Older sessions")} disabled={!status().older.enabled} onClick={() => archive.older()}>
            <Icon name="chevron-right" size={14} />{buttonText(width(), "Older") ? tr("Older") : ""}
          </button>
        </Show>
      </div>
      </Show>
      <Show when={status().error && !props.contentSearch}>
        <div class="archive-error" title={status().errorHint}>{status().error}</div>
      </Show>
    </div>
  );
}

/**
 * The archived date (ArchiveDatePicker): a button with the applied range, and
 * a panel where the quick ranges and the two dates change only the draft;
 * Apply range commits it, Cancel, Escape or a click outside drop it, and
 * focus returns to the picker button.
 */
function DatePicker() {
  let button: HTMLButtonElement | undefined;
  let panel: HTMLDivElement | undefined;
  const [draft, setDraft] = createSignal<Draft>({ period: 0, from: "", until: "" });
  const [place, setPlace] = createSignal({ left: 0, top: 0, width: 480 });

  const stage = (period: Period) => setDraft(stageDraft(period, archive.range(), new Date()));
  const edit = (edited: "from" | "until", value: string) => setDraft((d) => editDraft(d, edited, value, new Date()));
  const close = () => { setOpen(false); button?.focus(); };
  const apply = () => {
    const out = applyDraft(draft(), archive.range());
    if (!out) return;
    close();
    archive.setRange(out.range);
  };
  const viewport = () => ({ width: innerWidth, height: innerHeight });
  const show = () => {
    stage(archive.range().period);
    setPlace(panelPlace(button!.getBoundingClientRect(), 0, viewport()));
    setOpen(true);
  };

  createEffect(() => {
    if (!open() || !panel || !button) return;
    // Below the button when it fits, else above it; inside the window.
    setPlace(panelPlace(button.getBoundingClientRect(), panel.offsetHeight, viewport()));
    panel.querySelector<HTMLInputElement>("input")?.focus();
    // The press only closes the panel and focus returns to the picker, as Qt's popup consumes it.
    const outside = (e: MouseEvent) => {
      if (panel?.contains(e.target as Node) || button?.contains(e.target as Node)) return;
      e.preventDefault();
      close();
    };
    // The popup keeps the keyboard: Escape closes it wherever the focus is,
    // before the list's own Escape (peek, selection); Tab and Shift+Tab wrap
    // inside it.
    const keys = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); return; }
      if (e.key !== "Tab" || !panel) return;
      const controls = [...panel.querySelectorAll<HTMLElement>("input:not([disabled]), button:not([disabled])")];
      const next = tabTarget(controls.length, controls.indexOf(document.activeElement as HTMLElement), e.shiftKey);
      e.preventDefault();
      if (next !== undefined) controls[next].focus();
    };
    const t = setTimeout(() => addEventListener("mousedown", outside));
    addEventListener("keydown", keys, true);
    onCleanup(() => { clearTimeout(t); removeEventListener("mousedown", outside); removeEventListener("keydown", keys, true); });
  });
  // Leaving the Archive filter unmounts the panel with the bar.
  onCleanup(() => setOpen(false));

  return (
    <>
      <button ref={button} class="archive-period" title={pickerLabel(archive.range())} aria-label={pickerLabel(archive.range())}
        aria-haspopup="dialog" aria-expanded={open()} onClick={() => (open() ? setOpen(false) : show())}>
        <Icon name="calendar" size={16} />
        <span class="ell">{rangeCaption(archive.range())}</span>
        <Icon name="chevron-down" size={14} />
      </button>
      {/* In the page's body: a transformed list panel (the strip's peek) would otherwise place a fixed panel. */}
      <Show when={open()}>
        <Portal>
        <div class="archive-pop" ref={panel} role="dialog" aria-label={tr("Archived date")} data-shortcut-context="none"
          style={{ left: `${place().left}px`, top: `${place().top}px`, width: `${place().width}px` }}
          onPointerDown={(e) => e.stopPropagation()}>
          <div class="archive-pop-title">{tr("Archived date")}</div>
          <div class="archive-pop-body">
            <div class="archive-pop-custom">
              <label class="archive-pop-label" for="archive-from">{tr("From")}</label>
              <input id="archive-from" class="field" type="date" min={MIN_DAY} max={MAX_DAY} aria-label={tr("Archived from, inclusive")}
                value={draft().from} onInput={(e) => edit("from", e.currentTarget.value)} />
              <label class="archive-pop-label" for="archive-until">{tr("Through")}</label>
              <input id="archive-until" class="field" type="date" min={MIN_DAY} max={MAX_DAY} aria-label={tr("Archived through, inclusive")}
                value={draft().until} onInput={(e) => edit("until", e.currentTarget.value)} />
              <div class="hint archive-pop-hint" classList={{ invalid: draftReversed(draft()) }}>{draftHint(draft())}</div>
              <div class="archive-pop-actions">
                <button type="button" class="btn primary" disabled={!draftValid(draft())} onClick={apply}>{tr("Apply range")}</button>
                <button type="button" class="btn" onClick={close}>{tr("Cancel")}</button>
              </div>
            </div>
            <div class="archive-pop-quick">
              <div class="archive-pop-label">{tr("Quick ranges")}</div>
              <For each={PERIODS}>
                {(caption, i) => (
                  <button type="button" class="archive-preset" aria-pressed={draft().period === i()} onClick={() => stage(i() as Period)}>{tr(caption)}</button>
                )}
              </For>
            </div>
          </div>
        </div>
        </Portal>
      </Show>
    </>
  );
}
