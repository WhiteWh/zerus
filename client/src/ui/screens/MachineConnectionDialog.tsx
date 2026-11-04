import { onCleanup, onMount } from "solid-js";
import { tr } from "../../i18n";
import type { MachinePrompt } from "../../store/machineEditor";
import { Icon } from "../lib/icons";

export default function MachineConnectionDialog(props: { prompt: MachinePrompt; close: (prompt: MachinePrompt) => void; confirm: () => void }) {
  let dialog!: HTMLDialogElement;
  let cancel!: HTMLButtonElement;
  const origin = document.activeElement;
  const original = props.prompt;
  const close = () => props.close(original);
  const remove = () => props.prompt.kind === "remove";
  onMount(() => { dialog.showModal(); cancel.focus(); });
  onCleanup(() => { dialog.close(); if (origin instanceof HTMLElement && origin.isConnected) origin.focus(); });
  return <dialog class="dlg" ref={dialog} aria-labelledby="machine-confirm-title" style={{ width: "460px" }} onCancel={close} onClose={close}>
    <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="machine-confirm-title">{tr(remove() ? "Remove machine?" : "Unsaved connection")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={close}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body"><div class="confirm-body"><p>{props.prompt.kind === "remove" ? tr("Remove %1 from this Zerus? Its agents keep running and its files stay unchanged.", props.prompt.alias) : tr("Discard the unsaved changes to this connection?")}</p></div></div>
    <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn solid-danger" onClick={props.confirm}>{tr(remove() ? "Remove machine" : "Discard")}</button><button ref={cancel} type="button" class="btn primary" autofocus onClick={close}>{tr("Cancel")}</button></div>
  </dialog>;
}
