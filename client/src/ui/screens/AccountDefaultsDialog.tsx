import {For,Show,createSignal,onMount,onCleanup} from "solid-js";
import {Icon} from "../lib/icons";
import type {AccountPrompt} from "../../store/accountManagement";
import type {AccountEditValues} from "../../bridge";
import {accountModelChoices,accountModelView,permissionExplanation} from "./account-defaults";
import { tr } from "../../i18n";

/** Mockup SettingsDialogs PermissionsDialog / ModelDialog; Qt account options. */
export default function AccountDefaultsDialog(props:{prompt:AccountPrompt;close(handle:string):void;confirm(values:AccountEditValues,handle:string):void}) {
  const view=props.prompt,permissions=view.kind==="permissions",catalog=view.models;
  const titleId=permissions ? "accounts-permissions-title" : "accounts-model-title";
  const [mode,setMode]=createSignal(view.permission_mode??"provider"),[model,setModel]=createSignal(catalog?.defaults.model??""),[effort,setEffort]=createSignal(catalog && accountModelView(catalog,catalog.defaults.model).efforts.includes(catalog.defaults.effort) ? catalog.defaults.effort : "");
  let dialog!:HTMLDialogElement;
  onMount(()=>{dialog.showModal();queueMicrotask(()=>dialog.querySelector<HTMLElement>("select")?.focus());});
  onCleanup(()=>{dialog.close();if(view.origin.isConnected)view.origin.focus();});
  const state=()=>catalog ? accountModelView(catalog,model()) : undefined;
  const choose=(value:string)=>{setModel(value);setEffort(catalog && value===catalog.defaults.model && accountModelView(catalog,value).efforts.includes(catalog.defaults.effort) ? catalog.defaults.effort : "");};
  const inherited=()=>catalog ? `${catalog.provider_defaults.model||tr("Provider model")}${catalog.provider_defaults.effort ? `, ${catalog.provider_defaults.effort} effort` : ""}` : "";
  const submit=(event:Event)=>{event.preventDefault();if(permissions)props.confirm({mode:mode()},view.handle);else if(state()?.canSave)props.confirm({model:model(),effort:state()?.effortEnabled?effort():""},view.handle);};
  return <dialog ref={dialog} class="dlg" style={{width:"480px"}} aria-labelledby={titleId} onCancel={event=>{event.preventDefault();props.close(view.handle);}} onClose={()=>props.close(view.handle)}>
    <form onSubmit={submit}>
      <div class="dlg-head"><Icon name={permissions?"shield-check":"sparkles"} size={18} class="accent"/><h2 id={titleId}>{permissions?tr("Account permissions"):tr("Default model and effort")}</h2><span class="grow"/><button type="button" class="icon-btn" aria-label={tr("Close")} onClick={()=>props.close(view.handle)}><Icon name="x" size={16}/></button></div>
      <div class="dlg-body"><p>{view.label} on {view.machine}</p>
        <Show when={permissions} fallback={<Show when={catalog}>{data=><>
          <div class="form-row"><label for="md-model">{tr("Model")}</label><select id="md-model" class="field" value={model()} onChange={event=>choose(event.currentTarget.value)}>
            <option value="">{tr("Provider settings")}</option><For each={accountModelChoices(data())}>{option=><option value={option.id}>{option.label}</option>}</For>
          </select></div>
          <div class="form-row"><label for="md-effort">{tr("Effort")}</label><select id="md-effort" class="field" value={effort()} disabled={!state()?.effortEnabled} onChange={event=>setEffort(event.currentTarget.value)}>
            <option value="">{tr("Provider default")}</option><For each={state()?.efforts}>{value=><option value={value}>{value}</option>}</For>
          </select></div>
          <Show when={data().provider_defaults.model||data().provider_defaults.effort}><p class="muted">Account settings: {inherited()}</p></Show>
          <p class="hint">{state()?.hint}</p><p class="hint">{tr("New conversations with this account on this machine. Existing conversations keep their model and effort. Explicit launch choices take precedence.")}</p>
        </>}</Show>}>
          <select class="field acct-perm-select" aria-label={tr("Permission mode")} value={mode()} onChange={event=>setMode(event.currentTarget.value)}><option value="provider">{tr("Default (provider settings)")}</option><option value="bypass">{tr("Bypass permissions")}</option></select>
          <p classList={{danger:mode()==="bypass"}}>{permissionExplanation(view,mode())}</p><p class="hint">{tr("New and resumed sessions only. Running sessions keep their mode.")}</p>
        </Show>
      </div>
      <div class="dlg-foot"><span class="spacer"/><button type="button" class="btn" onClick={()=>props.close(view.handle)}>{tr("Cancel")}</button><button type="submit" class="btn primary" disabled={!permissions&&!state()?.canSave}>{tr("Save")}</button></div>
    </form>
  </dialog>;
}
