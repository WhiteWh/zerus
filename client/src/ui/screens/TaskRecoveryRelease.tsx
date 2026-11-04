import { onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

/** TaskDialogs' task-release-recovery confirmation, with Cancel focused. */
export default function TaskRecoveryRelease(props: { close: () => void; release: () => void; origin: HTMLElement }) {
  let dialog!: HTMLDialogElement, cancel!: HTMLButtonElement;
  onMount(() => { dialog.showModal(); queueMicrotask(() => cancel.focus()); });
  onCleanup(() => { if (dialog.open) dialog.close(); queueMicrotask(() => { if (props.origin.isConnected) props.origin.focus(); }); });
  return <dialog ref={dialog} class="dlg" aria-labelledby="task-release-recovery-title" style={{ width: "460px" }} onCancel={props.close} onClose={props.close}>
    <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="task-release-recovery-title">{tr("Release message recovery")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={props.close}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body"><div class="confirm-body"><p>{tr("The previous message may already have reached the executor. Releasing this local record neither cancels nor resends it. Check the task and executor Activity before sending another message.")}</p></div></div>
    <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn solid-danger" onClick={props.release}>{tr("Release record")}</button><button ref={cancel} type="button" class="btn primary" autofocus onClick={props.close}>{tr("Cancel")}</button></div>
  </dialog>;
}
