import {createSignal} from "solid-js";
import type {Workspace} from "./mobile-page";
import {createMobileInvitationMemory,type ComputerTarget} from "../../../store/mobileInvitation";
import { trNoop, tr } from "../../../i18n";
export type {ComputerTarget} from "../../../store/mobileInvitation";
/** Private, transient credential. Only the original mounted page can copy it. */
export interface ComputerCode{handle:string;code:string;expires_at:number;target:ComputerTarget}
export interface ComputerCodeIo{
 open(target:ComputerTarget):Promise<string>;issue(handle:string):Promise<ComputerCode>;
 close(handle:string):Promise<unknown>;cancel(handle:string):Promise<unknown>;check(handle:string):Promise<unknown>;
 copy(text:string):Promise<unknown>;refresh():void;
}
export const COMPUTER_UNCONFIRMED=trNoop("Workspace operation was not confirmed. Refresh status before continuing. Verify that the matching computer connector is installed.");
const key=(t?:ComputerTarget)=>t?JSON.stringify([t.config_revision,t.server_url]):"";
const ready=(expires:number)=>{const date=new Date(expires*1000),time=`${String(date.getHours()).padStart(2,"0")}:${String(date.getMinutes()).padStart(2,"0")}`;return tr("Computer code ready until %1. Keep this computer online while the other computer joins.", time);};
export function createComputerCodes(io:ComputerCodeIo,now:()=>number=()=>Date.now()/1000,memory=createMobileInvitationMemory()){
 const [credential,setCredential]=createSignal<ComputerCode>(),[target,setTarget]=createSignal<ComputerTarget>(),[status,setStatus]=createSignal("");
 let generation=0,disposed=false;
 const pending=()=>{const value=memory.pending();return value&&key(value.target)===key(target())?value:undefined;};
 const code=()=>{const value=credential();return value&&pending()?.handle===value.handle?value:undefined;};
 const release=(handle:string)=>void io.close(handle).catch(()=>{});
 const forget=(handle?:string)=>{const value=memory.clear(handle);if(!handle||credential()?.handle===handle)setCredential(undefined);if(value)release(value.handle);};
 const unknown=(handle?:string)=>{forget(handle);if(!disposed){setStatus(tr(COMPUTER_UNCONFIRMED));io.refresh();}};
 const tick=()=>{const value=pending();if(value&&value.expires_at<=now()){forget(value.handle);if(!disposed)setStatus(tr("Computer code expired."));}};
 return {code,pending,busy:memory.busy,status:()=>status()||(pending()?ready(pending()!.expires_at):""),
  observe(workspace?:Workspace|null){if(disposed)return;const next=workspace?.state==="active"&&workspace.encrypted&&!workspace.pending?{config_revision:workspace.config_revision,server_url:workspace.server_url}:undefined;if(key(memory.target())!==key(next)){forget();memory.observe(next);}if(key(target())===key(next))return;generation++;setTarget(next);setCredential(undefined);if(status()!==COMPUTER_UNCONFIRMED)setStatus("");tick();},
  tick,
  async issue(){if(disposed||memory.busy()||!target()||pending())return;const current=generation,captured=target()!;let handle:string|undefined;memory.setBusy(true);setStatus("");try{handle=await io.open(captured);if(disposed||current!==generation)return;const value=await io.issue(handle);if(key(value.target)!==key(captured)||key(memory.target())!==key(captured)){release(value.handle);return;}memory.retain(value);if(!disposed&&current===generation){setCredential(value);setStatus(ready(value.expires_at));tick();}}catch{unknown();}finally{if(handle)release(handle);memory.setBusy(false);}},
  async copy(){const value=code();if(!value||value.expires_at<=now()||memory.busy()||disposed)return false;try{await io.copy(value.code);return !disposed&&code()===value;}catch{if(!disposed)setStatus(tr("Could not copy the computer code."));return false;}},
  async cancel(){const value=pending();if(!value||memory.busy()||disposed)return;const current=generation;memory.setBusy(true);memory.clear(value.handle);setCredential(undefined);try{await io.cancel(value.handle);if(!disposed&&current===generation)setStatus(tr("Code canceled. Computers already joined keep their access."));}catch{unknown(value.handle);}finally{release(value.handle);memory.setBusy(false);}},
  async check(){const value=pending();if(!value||memory.busy()||disposed)return;memory.setBusy(true);try{await io.check(value.handle);}catch{unknown(value.handle);}finally{memory.setBusy(false);}},
  dispose(){disposed=true;generation++;setCredential(undefined);setTarget(undefined);setStatus("");},
 };
}
