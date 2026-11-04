import {createSignal} from "solid-js";
import {accountDistributionOpen,accountDistributionApply,accountDistributionClose,accountDistributionProgress,type AccountDistributionForm,type AccountDistributionValues,type AccountDistributionProgress} from "../bridge";
import {notify} from "./actions";
import {accountsPage,loadAccounts,setAccountMachine,setRequestedProfile} from "./accounts";
import {accountSetup} from "./accountSetup";
import { tr } from "../i18n";
interface Port {open(account:string,profile:string):Promise<AccountDistributionForm>;apply(handle:string,values:AccountDistributionValues):Promise<AccountDistributionProgress[]>;close(handle:string):Promise<unknown>;follow(handle:string,receive:(row:AccountDistributionProgress)=>void):Promise<()=>void>;error(text:string):void;select(profile:string):void}
export interface DistributionPrompt extends AccountDistributionForm {origin:HTMLElement}
export function createAccountDistribution(port:Port) {
 const [prompt,setPrompt]=createSignal<DistributionPrompt>(),[busy,setBusy]=createSignal(false),[progress,setProgress]=createSignal<AccountDistributionProgress[]>([]),[notice,setNotice]=createSignal("");let generation=0,context:{account:string;profile:string}|undefined;
 const close=(handle?:string)=>{const v=prompt();if(handle&&v?.handle!==handle)return;generation++;setPrompt(undefined);if(v)void port.close(v.handle).catch(()=>{});};
 const run=async(form:AccountDistributionForm,values:AccountDistributionValues,current:number)=>{
  const allowed=new Set(values.hosts);let stop:(()=>void)|undefined;
  const update=(row:AccountDistributionProgress)=>{if(current!==generation||!allowed.has(row.host)||row.handle!==form.handle)return;setProgress(rows=>rows.map(old=>old.host!==row.host||old.handle!==row.handle||(!["pending","working"].includes(old.state)&&["pending","working"].includes(row.state))?old:row));};
  try {stop=await port.follow(form.handle,update);if(current!==generation){await port.close(form.handle);return;}const answer=await port.apply(form.handle,values);answer.slice(0,64).forEach(update);if(current!==generation)answer.filter(row=>row.state==="failed"||row.state==="unknown").forEach(row=>port.error(`${row.machine}: ${row.text}`));}
  catch(error){if(current===generation)setProgress(rows=>rows.map(row=>allowed.has(row.host)&&["pending","working"].includes(row.state)?{...row,state:"unknown",text:String(error)}:row));port.error(String(error));}
  finally{stop?.();setBusy(false);}
 };
 return {prompt,busy,progress,notice,close,clearNotice:()=>setNotice(""),
  async open(account:string,profile:string,origin:HTMLElement){if(busy())return;close();setNotice("");setProgress([]);const current=generation;setBusy(true);try{const view=await port.open(account,profile);if(current!==generation){await port.close(view.handle);return;}context={account,profile};setPrompt({...view,origin});}catch(error){if(current===generation)port.error(String(error));}finally{setBusy(false);}},
  async start(values:AccountDistributionValues,handle:string){const view=prompt();if(!view||view.handle!==handle||busy())return;setBusy(true);setProgress(values.hosts.slice(0,64).map(host=>({handle,host,machine:view.machines.find(m=>m.key===host)?.machine??host,state:"pending",text:"Waiting",saved:false,profile:null})));await run(view,values,generation);},
  async retry(host:string,values:AccountDistributionValues){const view=prompt(),scope=context;if(!view||!scope||busy()||progress().find(r=>r.host===host)?.state!=="failed")return;const current=generation;setBusy(true);let fresh:AccountDistributionForm;try{fresh=await port.open(scope.account,scope.profile);}catch(error){setBusy(false);port.error(String(error));return;}if(current!==generation){await port.close(fresh.handle);setBusy(false);return;}setProgress(rows=>rows.map(r=>r.host===host?{...r,handle:fresh.handle,state:"pending",text:"Waiting"}:r));await run(fresh,{...values,hosts:[host]},current);},
  done(handle:string){const view=prompt();if(!view||view.handle!==handle||busy())return;const added=progress().filter(p=>p.state==="added"||p.state==="needs-signin");if(added.length){const needs=added.filter(p=>p.state==="needs-signin").length;setNotice(`${view.label} added on ${added.length} machine${added.length===1?"":"s"}${needs?`; ${needs} still need sign-in`:""}.`);const first=added.find(p=>p.profile);if(first?.profile)port.select(first.profile);}close(handle);}
 };
}
export const accountDistribution=createAccountDistribution({open:accountDistributionOpen,apply:accountDistributionApply,close:accountDistributionClose,follow:accountDistributionProgress,error:text=>notify("error",text,true),select:profile=>{setAccountMachine("*");setRequestedProfile(profile);}});
export async function signInDistributedProfile(profile:string,origin:HTMLElement) {await loadAccounts();const account=accountsPage().accounts.find(a=>a.members.some(m=>m.key===profile));if(account)await accountSetup.open("login",origin,account.key,profile);else notify("error",tr("Accounts changed on this machine. Refresh before saving."),true);}
