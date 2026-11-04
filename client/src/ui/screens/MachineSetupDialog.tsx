import { onMount, onCleanup, Show } from "solid-js";
import { machineSetupEditor as setup, type SetupPrompt } from "../../store/machineSetup";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

export default function MachineSetupDialog(props: { prompt: SetupPrompt }) {
  let dialog!: HTMLDialogElement;let cancel!: HTMLButtonElement;
  const original = props.prompt, origin = document.activeElement;
  const install = original.kind === "install";
  const alias = original.kind === "install" ? original.reference.alias : "";
  const close = () => setup.close(original);
  onMount(() => { dialog.showModal();cancel.focus(); });
  onCleanup(() => { dialog.close();if (origin instanceof HTMLElement && origin.isConnected) origin.focus(); });
  return <dialog class="dlg" ref={dialog} style={{ width: install ? "520px" : "460px" }} aria-labelledby="machine-setup-title" onCancel={close} onClose={close}>
    <div class="dlg-head"><Icon name={install ? "download" : "circle-question-mark"} size={18} class="accent" /><h2 id="machine-setup-title">{install ? `Install HGS on ${alias}` : tr("Enable local sessions?")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={close}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body">
      <Show when={install} fallback={<div class="confirm-body"><p>{tr("Run agents on this computer too? Zerus stages the HGS CLI and a session host in your user profile and enables local sessions. To disable local sessions again, run hgs session-host uninstall.")}</p></div>}>
        <p>{tr("Build and install the HGS CLI on %1 using SSH. The newest stable Rust and tmux must already be installed there. Existing sessions and configuration are preserved.", alias)}</p>
        <div class="row" style={{ "margin-top": "12px" }}><input class="field mono grow" aria-label={tr("HGS source folder")} placeholder={tr("Default: ~/.local/src/hgs on this device")} value={setup.source()} disabled={setup.choosing()} onInput={(e) => setup.setSource(e.currentTarget.value)} /><button class="btn" disabled={setup.choosing()} onClick={() => void setup.pick(original)}>{tr("Choose HGS source folder…")}</button></div>
      </Show>
    </div>
    <div class="dlg-foot"><span class="spacer" /><button class="btn" ref={cancel} onClick={close}>{tr("Cancel")}</button><button class="btn primary" disabled={setup.choosing()} onClick={() => void setup.confirm(original)}>{install ? "Install HGS" : tr("Enable local sessions")}</button></div>
  </dialog>;
}
