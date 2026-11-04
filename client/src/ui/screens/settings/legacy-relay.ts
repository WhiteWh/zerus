import {createSignal} from "solid-js";
import { trNoop, tr } from "../../../i18n";
export const LEGACY_UNKNOWN=trNoop("Configuration was not confirmed saved. Reload to check; no connector restart was requested.");
export const WINDOWS_DRAINING=trNoop("Stop requested. The current phone request will finish before the connector exits. Reload status to confirm.");
export const OFFICIAL_RELAY="https://relay.zerus.dev";
export interface LegacyForm{handle:string;windows:boolean;configured:boolean;configured_relay:string;config:string;relay:string;directory:string;new_directory:string;status:string;implementation:string;protection:string;commands:[string,string][];running:boolean}
export interface LegacyFields{relay:string;replacement:string;directory:string}
export interface LegacyReply{form?:LegacyForm|null;notice:string;kind?:"ok"|"error"|"muted"}
export interface LegacyIo{open():Promise<LegacyForm>;apply(handle:string,request:{action:string;edit:LegacyFields}):Promise<LegacyReply>;close(handle:string):Promise<unknown>}
export function createLegacyRelay(io:LegacyIo){
 const [form,setForm]=createSignal<LegacyForm>(),[fields,setFields]=createSignal<LegacyFields>({relay:OFFICIAL_RELAY,replacement:"",directory:""}),[busy,setBusy]=createSignal(false),[notice,setNotice]=createSignal(""),[noticeKind,setNoticeKind]=createSignal<"ok"|"error"|"muted">("muted"),[handle,setHandle]=createSignal<string>();
 let disposed=false,generation=0;
 const release=(h?:string)=>{if(h)void io.close(h).catch(()=>{});};
 const load=async()=>{const version=++generation;release(handle());setHandle(undefined);setFields(f=>({...f,replacement:""}));try{const f=await io.open();if(disposed||version!==generation){release(f.handle);return false;}setHandle(f.handle);setForm(f);setFields({relay:f.relay,replacement:"",directory:f.directory});return true;}catch(e){if(!disposed&&version===generation){setNotice(typeof e==="string"?e:tr(LEGACY_UNKNOWN));setNoticeKind("error");}return false;}};
 return{form,fields,setFields,busy,notice,noticeKind,ready:()=>!busy()&&!!handle(),
  async reload(){if(busy()||disposed)return;setBusy(true);setNotice("");try{await load();}finally{setBusy(false);}},
  async apply(action:string){if(busy()||disposed||!handle())return;const h=handle()!;setHandle(undefined);const edit=fields(),version=generation;setFields(f=>({...f,replacement:""}));setBusy(true);setNotice("");try{const result=await io.apply(h,{action,edit});if(disposed||version!==generation){release(result.form?.handle);return;}if(!disposed&&version===generation){if(result.form){setHandle(result.form.handle);setForm(result.form);}else if(result.notice!==WINDOWS_DRAINING&&!await load())return;setNotice(result.notice);setNoticeKind(result.kind??(result.notice.startsWith("Configuration saved privately.")?"ok":"muted"));}}catch(e){if(!disposed&&version===generation){setNotice(typeof e==="string"?e:tr(LEGACY_UNKNOWN));setNoticeKind("error");}}finally{release(h);setBusy(false);}},
  dispose(){disposed=true;generation++;release(handle());setHandle(undefined);setForm(undefined);setFields({relay:OFFICIAL_RELAY,replacement:"",directory:""});}
 };
}
