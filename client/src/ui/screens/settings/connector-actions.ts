import {createSignal} from "solid-js";
import type {Workspace} from "./mobile-page";
import {workspaceTarget,WORKSPACE_UNKNOWN,WORKSPACE_CHANGED,type WorkspaceTarget} from "./workspace-enrollment";
import { trNoop, tr } from "../../../i18n";
export type ConnectorKind="start"|"stop";
export interface ConnectorForm{handle:string;target:WorkspaceTarget;kind:ConnectorKind}
export interface ConnectorIo{open(target:WorkspaceTarget,kind:ConnectorKind):Promise<ConnectorForm>;apply(handle:string):Promise<{notice:string}>;close(handle:string):Promise<unknown>;refresh():void}
export const canStartConnector=(w?:Workspace|null)=>!!w&&w.encrypted&&w.state==="active"&&!w.pending&&!w.running&&w.connector_state==="stopped";
export const canStopConnector=(w?:Workspace|null)=>!!w&&w.encrypted&&["active","retired"].includes(w.state)&&w.running&&w.connector_state==="running"&&!!w.stop_supported&&!!w.generation;
export const CONNECTOR_DRAINING=trNoop("The connector is draining its current request. Refresh status to confirm it has stopped.");
const key=(target:WorkspaceTarget)=>JSON.stringify(target);
export function createConnectorActions(io:ConnectorIo){
 const [busy,setBusy]=createSignal(false),[notice,setNotice]=createSignal("");
 let current:Workspace|undefined|null,disposed=false,generation=0,admitting:WorkspaceTarget|undefined,draining:WorkspaceTarget|undefined;
 const release=(handle:string)=>void io.close(handle).catch(()=>{});
 const run=async(form:ConnectorForm)=>{const version=generation;setBusy(true);try{const result=await io.apply(form.handle);if(!disposed&&version===generation){draining=result.notice===CONNECTOR_DRAINING?form.target:undefined;setNotice(result.notice);}}catch{if(!disposed&&version===generation){setNotice(tr(WORKSPACE_UNKNOWN));io.refresh();}}finally{release(form.handle);setBusy(false);}};
 return {busy,notice,
  observe(workspace?:Workspace|null){current=workspace;if(draining&&workspace){const same=workspace.config_revision===draining.config_revision&&workspace.node_id===draining.node_id&&workspace.server_url===draining.server_url;if(same&&!workspace.running&&workspace.connector_state==="stopped"){draining=undefined;setNotice(tr("Connector stopped."));}else if(!same||(workspace.running&&workspace.generation!==draining.generation)){draining=undefined;setNotice(tr(WORKSPACE_CHANGED));}}const target=workspace?workspaceTarget(workspace):undefined;if(admitting&&(!target||key(target)!==key(admitting))){generation++;setNotice(tr(WORKSPACE_CHANGED));}},
  async request(kind:ConnectorKind,_origin:HTMLElement){if(disposed||busy()||!(kind==="start"?canStartConnector(current):canStopConnector(current)))return;const target=workspaceTarget(current!),version=generation;admitting=target;draining=undefined;setBusy(true);setNotice("");try{const form=await io.open(target,kind);if(disposed||version!==generation){release(form.handle);return;}admitting=undefined;if(form.kind!==kind||key(form.target)!==key(target)){release(form.handle);setNotice(tr(WORKSPACE_CHANGED));return;}await run(form);}catch{if(!disposed&&version===generation){setNotice(tr(WORKSPACE_UNKNOWN));io.refresh();}}finally{admitting=undefined;setBusy(false);}},
  dispose(){disposed=true;generation++;admitting=undefined;draining=undefined;current=undefined;}
 };
}
export type ConnectorActions=ReturnType<typeof createConnectorActions>;
