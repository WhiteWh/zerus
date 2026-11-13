import { Show, onCleanup, onMount } from "solid-js";
import { sessionModels as models } from "../../store/sessionModels";
import { AccountSwitchChip } from "./AccountSwitchChip";
import { ModelSettingsContent } from "./ModelSettingsDialog";
import { Icon } from "../lib/icons";
import type { Session } from "./types";
import { tr } from "../../i18n";
import "../styles/newsession.css";

export default function ProviderSelectionDialog(props: { session: Session; anchor: () => HTMLButtonElement | undefined; close: () => void }) {
  let dialog!: HTMLDialogElement;
  onMount(() => {
    dialog.showModal();
    (dialog.querySelector<HTMLElement>('input,[role="combobox"],.acct-switch button') ?? dialog.querySelector<HTMLElement>("button"))?.focus();
  });
  onCleanup(() => {
    models.close();
    if (dialog.open) dialog.close();
    queueMicrotask(() => props.anchor()?.focus());
  });
  const outside = (event: PointerEvent) => {
    if (event.target !== dialog) return;
    const box = dialog.getBoundingClientRect();
    if (event.clientX < box.left || event.clientX > box.right || event.clientY < box.top || event.clientY > box.bottom) props.close();
  };
  return (
    <dialog ref={dialog} class="dlg cmp-settings-dialog provider-selection-dialog" aria-labelledby="provider-selection-title" onPointerDown={outside}
      onCancel={(event) => { event.preventDefault(); props.close(); }}>
      <div class="dlg-head"><Icon name="sparkles" size={18} class="accent" /><h2 id="provider-selection-title">{tr("Account")} — {tr("Model and effort")}</h2><span class="grow" />
        <button type="button" class="icon-btn" aria-label={tr("Close")} title={tr("Close")} onClick={props.close}><Icon name="x" size={16} /></button>
      </div>
      <div class="dlg-body provider-selection-account-row">
        <div class="form-row"><label>{tr("Account")}</label><AccountSwitchChip session={props.session} onSwitch={props.close} /></div>
      </div>
      <Show when={models.view()} fallback={
        <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button></div>
      }>
        <ModelSettingsContent close={props.close} />
      </Show>
    </dialog>
  );
}
