import {createSignal} from "solid-js";
import type {PhoneList} from "./mobile-page";
import { tr } from "../../../i18n";
export interface PhoneTarget{host:string;workspace_id:string;workspace_name:string;server_url:string;encrypted:boolean}
export interface PhoneForm{handle:string;target:PhoneTarget;phone:string|null;name:string|null}
/** Transient credential: never put this in settings, notices, logs or proof. */
export interface PhoneCode{pair_code:string;pair_url:string;expires_at:number;workspace_name:string;encrypted:boolean;server_url:string;qr:boolean[];size:number}
export type PhonePrompt={id:number;kind:"pair";targets:PhoneTarget[];selected:number;origin:HTMLElement}|{id:number;kind:"revoke";form:PhoneForm;origin:HTMLElement};
export interface PhoneIo{open(kind:"invite"|"revoke",target:PhoneTarget,phone?:string):Promise<PhoneForm>;close(handle:string):Promise<unknown>;invite(handle:string):Promise<PhoneCode>;revoke(handle:string):Promise<unknown>;changed():void}
export const targetOf=(list:PhoneList):PhoneTarget=>({host:list.host,workspace_id:list.workspace_id,workspace_name:list.workspace_name,server_url:list.server_url,encrypted:list.encrypted});
export const targetKey=(t:PhoneTarget)=>JSON.stringify([t.host,t.workspace_id,t.server_url.replace(/\/+$/g,"")]);
export const phoneKey=(t:PhoneTarget,phone:string)=>JSON.stringify([targetKey(t),phone]);
export function createPhoneActions(io:PhoneIo){
 const [prompt,setPrompt]=createSignal<PhonePrompt>(),[busy,setBusy]=createSignal(false),[notice,setNotice]=createSignal("");let generation=0,next=0;
 const close=(id?:number)=>{const current=prompt();if(id!==undefined&&current?.id!==id)return;generation++;setPrompt(undefined);if(current?.kind==="revoke")void io.close(current.form.handle).catch(()=>{});};
 return {prompt,busy,notice,close,
  pair(targets:PhoneTarget[],selected:number,origin:HTMLElement){if(busy()||!targets.length)return;close();setNotice("");const at=Math.max(0,Math.min(selected,targets.length-1)),shown=targets.slice(0,64);if(at>=64)shown[63]=targets[at];setPrompt({id:++next,kind:"pair",targets:shown,selected:at>=64?63:at,origin});},
  async invite(target:PhoneTarget,id:number){if(prompt()?.id!==id||busy())return;const current=generation;setBusy(true);let form:PhoneForm|undefined;try{form=await io.open("invite",target);if(current!==generation)return;const result=await io.invite(form.handle);return current===generation?result:undefined;}finally{if(form)void io.close(form.handle).catch(()=>{});setBusy(false);}},
  async openRevoke(target:PhoneTarget,phone:string,origin:HTMLElement){if(busy())return;close();setNotice("");const current=generation;setBusy(true);try{const form=await io.open("revoke",target,phone);if(current!==generation){await io.close(form.handle);return;}setPrompt({id:++next,kind:"revoke",form,origin});}catch{if(current===generation)setNotice(tr("This mobile connection changed. Refresh the phone list and try again."));}finally{setBusy(false);}},
  async revoke(id:number){const view=prompt();if(view?.kind!=="revoke"||view.id!==id||busy())return;const current=generation;setBusy(true);try{await io.revoke(view.form.handle);if(current===generation){setNotice(`${view.form.name} revoked.`);close(id);io.changed();}}catch(error){if(current===generation){const detail=String(error).includes("This mobile connection changed.")?tr("This mobile connection changed"):"the relay did not answer";setNotice(tr("Could not confirm revoking %1: %2. Refresh to check whether it is still listed before trying again.", view.form.name ?? "", detail));close(id);}}finally{setBusy(false);}},
 };
}
export type PhoneActions=ReturnType<typeof createPhoneActions>;
