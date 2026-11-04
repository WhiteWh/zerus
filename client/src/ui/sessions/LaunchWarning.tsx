import { For, Show, createEffect, on } from "solid-js";
import { tr } from "../../i18n";
import { Icon } from "../lib/icons";
import { createConfirm, WARNING_BUTTONS, WARNING_TITLE } from "./newsession-view";

/** The local Windows-session warning: an alert with Launch anyway and Cancel, Cancel the default and focused. */
export function LaunchWarning(props: { confirm: ReturnType<typeof createConfirm> }) {
  let el!: HTMLDialogElement;
  createEffect(on(() => props.confirm.text(), (text) => {
    if (text === undefined) { if (el.open) el.close(); return; }
    if (!el.open) el.showModal();
    queueMicrotask(() => el.querySelector<HTMLElement>("[data-initial]")?.focus());
  }));
  return (
    <dialog class="dlg ns-warning" style={{ width: "min(480px, calc(100vw - 48px))" }} ref={el} role="alertdialog" aria-labelledby="ns-warning-title" aria-describedby="ns-warning-text"
      onCancel={(e) => { e.preventDefault(); props.confirm.cancel(); }} onClose={() => props.confirm.cancel()}>
      <Show when={props.confirm.text()}>{(text) => (
        <>
          <div class="dlg-head"><Icon name="triangle-alert" size={18} class="warn" /><h2 id="ns-warning-title">{tr(WARNING_TITLE)}</h2></div>
          <div class="dlg-body"><p class="confirm-body" id="ns-warning-text">{text()}</p></div>
          <div class="dlg-foot">
            <span class="spacer" />
            <For each={WARNING_BUTTONS}>{(button) => (
              <button type="button" class={button.initial ? "btn primary" : "btn"} data-initial={button.initial || undefined} autofocus={button.initial}
                onClick={() => props.confirm.answer(button.answer)}>{tr(button.label)}</button>
            )}</For>
          </div>
        </>
      )}</Show>
    </dialog>
  );
}
