import {Show,createSignal,createEffect,onMount,onCleanup,lazy,Suspense} from "solid-js";
import {invoke} from "@tauri-apps/api/core";
import {Icon} from "../../lib/icons";
import {toast} from "../../../store/ui";
import {Group,Row,Select,Actions,Status,Lines} from "./ui";
import type {View} from "./mobile-page";
import {createWorkspaceEnrollment,enrollmentLabel,STOP_UNAVAILABLE,relayError,codeError,type WorkspaceMode} from "./workspace-enrollment";
import {createConnectorActions,canStartConnector,canStopConnector} from "./connector-actions";
import ComputerInvitations from "./ComputerInvitations";
import { tr, trNoop } from "../../../i18n";
const WorkspaceConfirm=lazy(()=>import("./WorkspaceConfirm"));
export default function WorkspacePanel(props:{view?:View;refresh:(afterAction?:boolean)=>void}){
 let submit!:HTMLButtonElement;
 const read=()=>props.view?.workspace,reading=()=>!!props.view?.workspace_busy;
 const actions=createWorkspaceEnrollment({open:(target,request)=>invoke("mobile_workspace_open",{target,request}),check:(target,allowLocal)=>invoke("mobile_workspace_check_open",{target,allowLocal}),apply:handle=>invoke("mobile_workspace_apply",{handle}),advance:handle=>invoke("mobile_workspace_continue",{handle}),close:handle=>invoke("mobile_workspace_close",{handle}),refresh:()=>props.refresh(true),connected:text=>toast("Workspace",text)});
 const connector=createConnectorActions({open:(target,kind)=>invoke("mobile_connector_open",{target,kind}),apply:handle=>invoke("mobile_connector_apply",{handle}),close:handle=>invoke("mobile_connector_close",{handle}),refresh:()=>props.refresh(true)});
 const [notice,setNotice]=createSignal("");
 createEffect(()=>{actions.busy();setNotice(actions.notice());});createEffect(()=>{connector.busy();setNotice(connector.notice());});
 createEffect(()=>{actions.observe(read());connector.observe(read());});onCleanup(()=>connector.dispose());
 onMount(()=>{const timer=setInterval(()=>void actions.advance(),1000);onCleanup(()=>clearInterval(timer));});onCleanup(()=>actions.dispose());
 const busy=()=>reading()||actions.busy()||connector.busy(),changing=()=>!!read()&&["active","legacy","retired"].includes(read()!.state),legacyPending=()=>read()?.pending_state==="legacy-retirement-unconfirmed";
 const stopUnavailable=()=>!!read()?.running&&!read()?.stop_supported;
 const writable=()=>!stopUnavailable()&&!!read()&&!busy()&&!read()?.pending&&read()?.state!=="pending-activation"&&!actions.stale();
 return <>
  <Group class="ws-panel"><div class="ws-summary">
   <span class="ws-icon" classList={{ok:read()?.state==="active",warn:read()?.state==="legacy"}}><Icon name={read()?.state==="active"?"lock":read()?.state==="legacy"?"triangle-alert":"network"} size={18}/></span>
   <div class="col grow" style={{gap:"2px"}}><Show when={read()} fallback={<Status kind={props.view?.workspace_error?"error":"busy"}>{props.view?.workspace_error?tr("Workspace status is unconfirmed"):"Workspace: checking…"}</Status>}>{w=><><Lines class="ws-title" text={w().summary}/><Show when={w().connection}><span class="muted">{w().connection}</span></Show></>}</Show></div>
  </div>
  <Show when={props.view?.workspace_error}>{text=><Status kind="error">{text()}</Status>}</Show>
  <Show when={stopUnavailable()}><Status kind="warn">{tr(STOP_UNAVAILABLE)}</Status></Show>
  <Actions><Show when={read()?.controls.change&&!actions.open()}><button class="btn" disabled={!writable()} onClick={()=>actions.begin()}>{tr("Change workspace…")}</button></Show>
   <button class="btn" disabled={busy()||!canStartConnector(read())} onClick={e=>void connector.request("start",e.currentTarget)}><Icon name="play" size={13}/>{tr("Start connector")}</button><button class="btn" disabled={busy()||!canStopConnector(read())} onClick={e=>void connector.request("stop",e.currentTarget)}><Icon name="square" size={12}/>{tr("Stop connector")}</button>
   <button class="btn" disabled={busy()||!props.view} onClick={()=>props.refresh()}><Icon name="refresh-cw" size={13}/>{tr("Refresh status")}</button>
  </Actions>
  <Show when={actions.open()&&!read()?.pending}>
   <form class="ws-enroll" onSubmit={e=>{e.preventDefault();void actions.submit(submit);}}><Row label={trNoop("Connection")} for="ws-mode"><Select id="ws-mode" value={actions.fields().mode} disabled={actions.busy()} onChange={mode=>actions.setFields(f=>({...f,mode:mode as WorkspaceMode}))} options={[{value:"create",label:tr("Create workspace")},{value:"join",label:tr("Join by code")}]}/></Row>
    <Row label={trNoop("Relay (HTTPS)")} for="ws-relay" hint={relayError(actions.fields())?<span class="danger">{relayError(actions.fields())}</span>:undefined}><input id="ws-relay" class="field mono set-wide-field" value={actions.fields().relay} readOnly={actions.busy()} onInput={e=>actions.setFields(f=>({...f,relay:e.currentTarget.value}))} aria-invalid={!!relayError(actions.fields())}/></Row>
    <Show when={actions.fields().mode==="join"}><Row label={trNoop("Computer invitation code")} for="ws-code" hint={actions.fields().code&&codeError(actions.fields())?<span class="danger">{codeError(actions.fields())}</span>:undefined}><input id="ws-code" class="field mono set-wide-field" type="password" autocomplete="off" autocapitalize="none" spellcheck={false} maxLength={4096} placeholder="ze1.…" value={actions.fields().code} readOnly={actions.busy()} onInput={e=>actions.setFields(f=>({...f,code:e.currentTarget.value}))}/></Row></Show>
    <Actions><button ref={submit} type="submit" class="btn primary" disabled={!writable()||!!relayError(actions.fields())||!!codeError(actions.fields())}>{enrollmentLabel(actions.fields().mode,changing())}</button><Show when={changing()}><button type="button" class="btn ghost" disabled={actions.busy()} onClick={()=>actions.cancel()}>{tr("Cancel")}</button></Show></Actions>
   </form>
  </Show>
  <Show when={read()?.pending}><Show when={legacyPending()} fallback={<><Status kind="warn">{read()!.pending}</Status><Actions><button class="btn small" disabled={busy()} onClick={e=>void actions.check(e.currentTarget)}>{read()!.controls.check??tr("Check connection")}</button></Actions></>}>
   <div class="notice warn"><Icon name="triangle-alert" size={14}/><div class="col" style={{gap:"8px"}}><span>{tr("The new encrypted workspace is ready. The old relay has not confirmed retirement. You can explicitly disconnect its local configuration before switching.")}</span><Actions><button class="btn small" disabled={busy()} onClick={e=>void actions.check(e.currentTarget)}>{tr("Continue connecting")}</button><button class="btn small danger" disabled={busy()} onClick={e=>void actions.check(e.currentTarget,true)}>{tr("Disconnect legacy locally and switch…")}</button></Actions></div></div>
  </Show></Show>
  <Show when={read()?.controls.invitations}><ComputerInvitations workspace={read()!} busy={busy()} refresh={props.refresh}/></Show>
  <Show when={notice()}><Status>{notice()}</Status></Show>
  </Group>
  <Show when={actions.prompt()}>{prompt=><Suspense><WorkspaceConfirm prompt={prompt()} actions={actions}/></Suspense>}</Show>
 </>;
}
