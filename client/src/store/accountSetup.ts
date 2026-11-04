import {createSignal} from "solid-js";
import {accountSetupOpen,accountSetupApply,accountSetupClose,type AccountSetupKind,type AccountSetupView,type AccountSetupValues,type AccountSetupResult} from "../bridge";
import {notify} from "./actions";
import {setAccountMachine,setRequestedProfile} from "./accounts";
export interface SetupPrompt extends AccountSetupView {origin:HTMLElement;preferredHost:string}
interface Port {open(kind:AccountSetupKind,account:string,profile:string):Promise<AccountSetupView>;apply(handle:string,values:AccountSetupValues):Promise<AccountSetupResult>;close(handle:string):Promise<unknown>;error(text:string):void;select(profile:string):void}
export function createAccountSetup(port:Port) {
 const [prompt,setPrompt]=createSignal<SetupPrompt>(),[busy,setBusy]=createSignal(false),[notice,setNotice]=createSignal("");let generation=0;
 const close=(handle?:string)=>{const p=prompt();if(handle&&p?.handle!==handle)return;generation++;setPrompt(undefined);if(p)void port.close(p.handle).catch(()=>{});};
 const apply=async(view:AccountSetupView,values:AccountSetupValues)=>{const keepKey=view.kind==="key"||!!values.api_key;if(!keepKey)setPrompt(undefined);setBusy(true);try {const result=await port.apply(view.handle,values);setBusy(false);if(result.profile)port.select(result.profile);setNotice(result.notices.join("\n"));result.errors.forEach(text=>port.error(text));}catch(e){port.error(String(e));}finally{setBusy(false);if(keepKey&&prompt()?.handle===view.handle)setPrompt(undefined);}};
 return {prompt,busy,notice,close,clearNotice:()=>setNotice(""),
  async open(kind:AccountSetupKind,origin:HTMLElement,account="",profile="",preferredHost="*") {
   if(busy())return;close();setNotice("");const current=generation;setBusy(true);
   try {const view=await port.open(kind,account,profile);if(current!==generation){await port.close(view.handle);return;}if(["login","install","start"].includes(kind))await apply(view,{});else setPrompt({...view,origin,preferredHost});}
   catch(e){if(current===generation)port.error(String(e));}finally{setBusy(false);}
  },
  async confirm(values:AccountSetupValues,handle:string){const p=prompt();if(!p||busy()||p.handle!==handle)return;await apply(p,values);}
 };
}
export const accountSetup=createAccountSetup({open:accountSetupOpen,apply:accountSetupApply,close:accountSetupClose,error:text=>notify("error",text,true),select:profile=>{setAccountMachine("*");setRequestedProfile(profile);}});
