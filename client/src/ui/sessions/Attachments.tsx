import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Icon } from "../lib/icons";
import { composer } from "../../store/composer";
import { inTauri } from "../../bridge";
import type { AttachmentView } from "../../types";
import { keptItem } from "./composer-ui";
import { tr } from "../../i18n";

// The composer's attachments (MessageComposer::rebuildAttachments, 1008-1044;
// removeAttachment, 1046-1065), after the mockup's AttachmentsChip: the chip
// opens a list; hovering a row, or reaching it with an arrow key, shows a
// larger preview beside the list; only the row's remove button (or Delete,
// Backspace) removes it, and focus stays in the list. Undo restores the last
// removal while Rust keeps it. A click, Enter or Space opens a row's file
// (MessageComposer.cpp:1104-1117), closing the list first.

const kib = (bytes: number) => tr("%1 KiB", Math.max(1, Math.ceil(bytes / 1024)));
/** The draft file's `zerus-file` URL (Task 3b); outside the app there is none. */
const src = (a: AttachmentView) => (inTauri() ? convertFileSrc(a.path, "zerus-file") : "");
const image = (a: AttachmentView) => a.mime.startsWith("image/");

/** The chip's label: "{n} attached", or "{n}" in a narrow toolbar (Qt's short label). */
export const attachedLabel = (n: number, compact: boolean) => (compact ? String(n) : tr("%1 attached", n));

export function AttachmentsChip(props: { compact?: boolean }) {
  const items = () => composer.view()?.attachments ?? [];
  /** The rows' keys: a view's attachments are new objects every time, their references are not. */
  const references = createMemo(() => items().map((a) => a.reference), undefined, { equals: (a, b) => a.length === b.length && a.every((r, i) => r === b[i]) });
  const [open, setOpen] = createSignal(false);
  const [preview, setPreview] = createSignal<number>();
  // The toolbar clips its overflow, so the list is placed in the viewport above
  // the chip; the preview takes the side with room for it, else goes above.
  const [pos, setPos] = createSignal<{ left: number; bottom: number; side: "right" | "left" | "above" }>({ left: 0, bottom: 0, side: "right" });
  let chip!: HTMLButtonElement;
  let list: HTMLDivElement | undefined;
  // Focus alone never opens a preview: the pointer rests on a row, or an arrow key moved to it.
  let arrowed = false;
  /** The row to focus once Rust's view shows a removal. */
  let refocus: number | undefined;
  const place = () => {
    const r = chip.getBoundingClientRect();
    const left = Math.max(8, Math.min(r.left, innerWidth - 368));
    setPos({ left, bottom: innerHeight - r.top + 6, side: innerWidth - (left + 360) >= 344 ? "right" : left >= 344 ? "left" : "above" });
  };
  const close = (returnFocus = false) => { setOpen(false); setPreview(undefined); if (returnFocus) chip.focus(); };
  const rows = () => [...(list?.querySelectorAll<HTMLElement>(".att-row") ?? [])];
  /** Qt closes the popover, then opens the file (MessageComposer::openAttachment). */
  const openFile = (index: number) => { close(); void composer.openAttachment(index); };
  const remove = (index: number) => {
    if (composer.locked()) return;
    setPreview(undefined);
    refocus = index;
    void composer.remove(index);
  };
  // Stay in the list: the row that took this place, else the one before it, else Undo.
  createEffect(on(references, () => {
    if (refocus === undefined || !open()) return;
    const index = refocus;
    refocus = undefined;
    queueMicrotask(() => { const r = rows(); (r[Math.min(index, r.length - 1)] ?? list?.querySelector<HTMLElement>(".att-undo"))?.focus(); });
  }, { defer: true }));
  // The last attachment gone with nothing to undo: the list closes with the chip (Qt's setActive(count > 0)).
  createEffect(() => { if (open() && !items().length && !composer.view()?.removed) close(); });
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(true); return; }
    const r = rows(), i = r.indexOf(document.activeElement as HTMLElement);
    if ((e.key === "ArrowDown" || e.key === "ArrowUp") && r.length) {
      e.preventDefault();
      arrowed = true;
      r[(i + (e.key === "ArrowDown" ? 1 : -1) + r.length) % r.length]?.focus();
    }
  };
  const outside = (e: PointerEvent) => { if (open() && !list?.contains(e.target as Node) && !chip.contains(e.target as Node)) close(); };
  onMount(() => addEventListener("pointerdown", outside, true));
  onCleanup(() => removeEventListener("pointerdown", outside, true));
  const current = () => { const i = preview(); return i === undefined ? undefined : items()[i]; };
  return (
    <span class="att-anchor">
      <button ref={chip} type="button" class="tchip" classList={{ on: open() }} hidden={!items().length && !open()} aria-haspopup="dialog" aria-expanded={open()}
        aria-label={tr("%1 attached", items().length)} title={items().map((a) => `${a.reference} ${a.name}`).join("\n")}
        onClick={(e) => {
          setPreview(undefined);
          arrowed = false;
          place();
          setOpen(!open());
          if (open()) queueMicrotask(() => (e.detail === 0 ? rows()[0] ?? list : list)?.focus({ preventScroll: true }));
        }}>
        <Icon name="paperclip" size={13} />{attachedLabel(items().length, !!props.compact)}
      </button>
      <Show when={open()}>
        <div class={`att-pop pv-${pos().side}`} ref={list} tabindex="-1" role="dialog" aria-label={tr("Attachments")} data-shortcut-context="none" onKeyDown={onKey}
          style={{ left: `${pos().left}px`, bottom: `${pos().bottom}px` }}>
          <div class="att-head"><b>{tr("Attachments")}</b><span class="dim">{tr("%1 of 8, up to 10 MiB each", items().length)}</span></div>
          {/* Rows are keyed by their reference, so a new view (a minute tick) keeps them and their focus. */}
          <For each={references()}>{(reference, i) => {
            const a = keptItem(items, reference);
            return (
              <div class="att-row" tabindex="0" aria-label={a().name} title={tr("Open %1", a().name)}
                onMouseEnter={() => setPreview(i())} onMouseLeave={() => setPreview(undefined)} onFocus={() => { if (arrowed) setPreview(i()); }}
                onClick={(e) => { if (e.button === 0) openFile(i()); }}
                onKeyDown={(e) => {
                  if (e.target !== e.currentTarget) return;
                  if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); remove(i()); }
                  if (e.key === "Enter" || e.key === " ") { e.preventDefault(); openFile(i()); }
                }}>
                <span class="att-thumb" classList={{ file: !image(a()) }}>
                  <Show when={image(a())} fallback={<Icon name="file-text" size={16} />}><img src={src(a())} alt="" /></Show>
                </span>
                <span class="att-text"><span class="ell"><span class="att-ref">{reference}</span> {a().name}</span><span class="dim tabular">{kib(a().bytes)}</span></span>
                <button type="button" class="icon-btn small att-remove" aria-label={tr("Remove attachment %1", a().name)} title={tr("Remove")} disabled={composer.locked()}
                  onClick={(e) => { e.stopPropagation(); remove(i()); }}><Icon name="x" size={13} /></button>
              </div>
            );
          }}</For>
          <Show when={composer.view()?.removed}>{(name) => (
            <div class="att-removed"><span class="ell">{tr("Removed %1", name())}</span><span class="grow" />
              <button type="button" class="btn small ghost att-undo" disabled={composer.locked()} onClick={() => { refocus = 0; void composer.undo(); }}>{tr("Undo")}</button></div>
          )}</Show>
          <Show when={!items().length && !composer.view()?.removed}><div class="dim att-empty">{tr("No attachments.")}</div></Show>
          <Show when={current()}>{(a) => (
            <div class="att-preview" aria-hidden="true">
              <Show when={image(a())} fallback={<pre class="mono">{a().excerpt ?? ""}</pre>}><img src={src(a())} alt="" /></Show>
              <div class="att-preview-cap"><span class="ell">{a().name}</span><span class="dim tabular">{kib(a().bytes)}</span></div>
            </div>
          )}</Show>
        </div>
      </Show>
    </span>
  );
}
