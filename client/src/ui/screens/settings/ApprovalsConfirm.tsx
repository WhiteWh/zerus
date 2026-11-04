import { onCleanup, onMount } from "solid-js";
import { Icon } from "../../lib/icons";
import { APPROVALS_TITLE, APPROVALS_BODY, APPROVALS_CONFIRM, type createApprovalOptIn } from "./approval-opt-in";
import { tr } from "../../../i18n";
export default function ApprovalsConfirm(props: { origin: HTMLElement; actions: ReturnType<typeof createApprovalOptIn> }) {
  let dialog!: HTMLDialogElement;
  const origin = props.origin;
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLButtonElement>(".dlg-foot [autofocus]")?.focus()); });
  onCleanup(() => { dialog.close(); if (origin.isConnected) origin.focus(); });
  return <dialog ref={dialog} class="dlg" style={{ width: "460px" }} aria-labelledby="approvals-enable-title"
    onCancel={(event) => { event.preventDefault(); props.actions.cancel(); }}>
    <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="approvals-enable-title">{tr(APPROVALS_TITLE)}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} disabled={props.actions.busy()} onClick={props.actions.cancel}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body"><div class="confirm-body"><p>{tr(APPROVALS_BODY)}</p></div></div>
    <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn solid-danger" disabled={props.actions.busy()} onClick={() => void props.actions.confirm()}>{tr(APPROVALS_CONFIRM)}</button><button type="button" class="btn primary" autofocus disabled={props.actions.busy()} onClick={props.actions.cancel}>{tr("Cancel")}</button></div>
  </dialog>;
}
