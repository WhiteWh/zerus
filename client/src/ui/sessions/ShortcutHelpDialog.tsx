import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { closeShortcutHelp, customizeShortcutHelp, shortcutHelpOrigin } from "../../store/goTo";
import { filterShortcutRows, shortcutHelpRows } from "./shortcutHelp";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

export function ShortcutHelpDialog() {
  let dialog: HTMLDialogElement | undefined;
  let filter: HTMLInputElement | undefined;
  const [needle, setNeedle] = createSignal("");
  const rows = createMemo(() => filterShortcutRows(shortcutHelpRows(), needle()));
  const groups = createMemo(() => [...new Set(rows().map((row) => row.group))]);
  createEffect(() => {
    if (!shortcutHelpOrigin() || !dialog || dialog.open) return;
    setNeedle("");
    dialog.show();
    queueMicrotask(() => filter?.focus());
  });
  return (
    <Show when={shortcutHelpOrigin()}>
      <dialog class="dlg shortcut-help" ref={dialog} aria-labelledby="shortcut-help-title" onCancel={(event) => { event.preventDefault(); closeShortcutHelp(); }} onClose={() => closeShortcutHelp()}>
        <div class="dlg-head"><Icon name="keyboard" size={18} class="accent" /><h2 id="shortcut-help-title">{tr("Keyboard shortcuts")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={() => closeShortcutHelp()}><Icon name="x" size={16} /></button></div>
        <div class="dlg-body">
          <input ref={filter} class="field shortcut-help-filter" placeholder={tr("Filter shortcuts")} value={needle()} onInput={(event) => setNeedle(event.currentTarget.value)}
            onKeyDown={(event) => { if (event.key === "Escape") { event.preventDefault(); closeShortcutHelp(); } }} />
          <div class="shortcut-help-list">
            <For each={groups()}>{(group) => (
              <section class="sc-group">
                <div class="eyebrow">{group}</div>
                <For each={rows().filter((row) => row.group === group)}>{(row) => (
                  <div class="kv"><span>{row.title}</span><span class="kbd">{row.chord}</span></div>
                )}</For>
              </section>
            )}</For>
          </div>
        </div>
        <div class="dlg-foot"><span class="hint">{tr("Chords marked ✓ also work while Terminal has focus.")}</span><span class="spacer" /><button class="btn" data-initial onClick={customizeShortcutHelp}>{tr("Customize…")}</button></div>
      </dialog>
    </Show>
  );
}
