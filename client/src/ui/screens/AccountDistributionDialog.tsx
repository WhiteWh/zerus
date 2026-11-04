import {For,Show,createEffect,createMemo,createSignal,on,onMount,onCleanup} from "solid-js";
import {createVirtualizer} from "@tanstack/solid-virtual";
import {Icon} from "../lib/icons";
import {MachineTile} from "../components/MachineTile";
import {MachineLabel} from "../components/MachineLabel";
import {osOf} from "../../store/overview";
import {accountDistribution,signInDistributedProfile,type DistributionPrompt} from "../../store/accountDistribution";
import type {AccountDistributionValues,AccountDistributionState} from "../../bridge";
import { tr } from "../../i18n";
const slug=(s:string)=>s.toLowerCase().replace(/[^a-z0-9]+/g,"-").replace(/^-|-$/g,"").slice(0,48)||"account";
const RANK:Record<AccountDistributionState,number>={failed:0,unknown:1,"needs-signin":2,working:3,pending:4,added:5};
/** Approved AddToMachine.tsx layout, driven by a bounded native snapshot. */
export default function AccountDistributionDialog(props:{prompt:DistributionPrompt}) {
 const view=props.prompt,state=accountDistribution;
 const [step,setStep]=createSignal<1|2|3>(1),[query,setQuery]=createSignal(""),[picked,setPicked]=createSignal<string[]>([]),[availableOnly,setAvailableOnly]=createSignal(false),[method,setMethod]=createSignal<AccountDistributionValues["method"]>(view.provider==="dsh"?"apikey":view.sources.length?"copy":"signin"),[useSaved,setUseSaved]=createSignal(true),[source,setSource]=createSignal(view.sources[0]?.key??""),[name,setName]=createSignal(view.label),[profileId,setProfileId]=createSignal(view.provider==="dsh"?"native-dsh":slug(`${view.provider}-${view.label}`)),[apiKey,setApiKey]=createSignal("");
 const blocked=(host:string)=>view.machines.find(m=>m.key===host)?.blocked;
 const shown=createMemo(()=>{const q=query().trim().toLowerCase();return view.machines.filter(m=>(!q||`${m.machine} ${m.platform}`.toLowerCase().includes(q))&&(!availableOnly()||!m.blocked));});
 const selectable=()=>shown().filter(m=>!m.blocked),toggle=(host:string)=>setPicked(rows=>rows.includes(host)?rows.filter(h=>h!==host):[...rows,host]);
 const savedCount=()=>view.machines.filter(m=>picked().includes(m.key)&&m.saved).length;
 const idValid=()=>/^[a-z0-9][a-z0-9-]{0,47}$/.test(profileId());
 const canAdd=()=>picked().length>0&&!!name().trim()&&idValid()&&(method()!=="apikey"||!!apiKey().trim())&&(method()!=="copy"||!!source()||useSaved()&&savedCount()===picked().length);
 const values=():AccountDistributionValues=>({hosts:picked(),method:method(),use_saved:useSaved(),source:source(),label:name().trim(),profile_id:profileId(),api_key:apiKey().trim()});
 const start=()=>{if(!canAdd())return;setStep(3);void state.start(values(),view.handle);};
 const finished=()=>!state.busy()&&state.progress().length>0&&state.progress().every(p=>p.state!=="pending"&&p.state!=="working");
 const count=(s:AccountDistributionState)=>state.progress().filter(p=>p.state===s).length;
 const ordered=()=>finished()?[...state.progress()].sort((a,b)=>RANK[a.state]-RANK[b.state]):state.progress();
 let dialog!:HTMLDialogElement;let scroller:HTMLDivElement|undefined;
 const virtualizer=createVirtualizer({get count(){return shown().length;},getScrollElement:()=>scroller??null,estimateSize:()=>48,overscan:6});
 createEffect(on(()=>shown().length,()=>{scroller?.scrollTo({top:0});virtualizer.measure();},{defer:true}));
 onMount(()=>{dialog.showModal();queueMicrotask(()=>dialog.querySelector<HTMLInputElement>("input")?.focus());});
 onCleanup(()=>{setApiKey("");dialog.close();if(view.origin.isConnected)view.origin.focus();});
 return <dialog ref={dialog} class="dlg" style={{width:"600px"}} aria-labelledby="accounts-add-to-machine-title" onCancel={e=>{e.preventDefault();state.close(view.handle);}} onClose={()=>state.close(view.handle)}>
  <div class="dlg-head"><Icon name="user-round-cog" size={18} class="accent"/><h2 id="accounts-add-to-machine-title">{tr("Add %1 to machines", view.label)}</h2><span class="grow"/><button type="button" class="icon-btn" aria-label={tr("Close")} onClick={()=>state.close(view.handle)}><Icon name="x" size={16}/></button></div>
  <div class="dlg-body">
   <ol class="atm-steps" aria-label={tr("Steps")}><For each={["Machines",tr("How to add"),"Add"]}>{(label,i)=><li classList={{on:step()===i()+1,done:step()>i()+1}}><b>{i()+1}</b>{label}</li>}</For></ol>
   <Show when={step()===1}>
    <div class="atm-tools"><input class="field atm-search" type="search" placeholder={`Search ${view.machines.length} machines`} aria-label={tr("Search machines")} value={query()} onInput={e=>setQuery(e.currentTarget.value)}/><label class="check"><input type="checkbox" checked={availableOnly()} onChange={e=>setAvailableOnly(e.currentTarget.checked)}/>{tr("Available only")}</label><button class="btn small ghost" disabled={!selectable().length} onClick={()=>setPicked([...new Set([...picked(),...selectable().map(m=>m.key)])])}>{tr("Select all available")}</button></div>
    <div class="atm-list" role="listbox" aria-multiselectable="true" aria-label={tr("Machines")} ref={el=>{scroller=el;requestAnimationFrame(()=>virtualizer._willUpdate());}}>
     <Show when={shown().length} fallback={<div class="atm-empty dim">{tr("No machine matches “%1”.", query())}</div>}><div style={{height:`${virtualizer.getTotalSize()}px`,position:"relative"}}><For each={virtualizer.getVirtualItems()}>{item=>{const m=()=>shown()[item.index];return <Show when={m()}><div class="atm-row" role="option" tabIndex={m().blocked?-1:0} aria-selected={picked().includes(m().key)} aria-disabled={!!m().blocked} classList={{on:picked().includes(m().key),blocked:!!m().blocked,has:m().present}} style={{transform:`translateY(${item.start}px)`}} onClick={()=>{if(!blocked(m().key))toggle(m().key);}} onKeyDown={e=>{if((e.key==="Enter"||e.key===" ")&&!blocked(m().key)){e.preventDefault();toggle(m().key);}}}>
      <span class="atm-check" aria-hidden="true"><Show when={m().present||picked().includes(m().key)}><Icon name="check" size={12}/></Show></span><MachineTile os={osOf(m().platform)} state={m().blocked==="Not connected"?"offline":"online"} size={28}/><span class="atm-name"><MachineLabel alias={m().machine}/><Show when={m().version}><span class="dim tabular">hgs {m().version}</span></Show></span><span class="atm-why" classList={{ok:!m().blocked}}>{m().blocked??(m().saved?tr("Saved sign-in available"):"")}</span>
     </div></Show>;}}</For></div></Show>
    </div>
   </Show>
   <Show when={step()===2}>
    <p>{picked().length===1?tr("%1 will get a separate profile on %2. Other accounts and sessions there stay as they are; after sign-in, the provider identity decides which account a profile belongs to.",view.label,view.machines.find(m=>m.key===picked()[0])?.machine??""):tr("%1 will get a separate profile on %2 machines. Other accounts and sessions there stay as they are; after sign-in, the provider identity decides which account a profile belongs to.",view.label,picked().length)}</p>
    <Show when={method()==="apikey"} fallback={<div class="col atm-how" role="radiogroup" aria-label={tr("How to add")}>
     <Show when={savedCount()}><label class="check"><input type="checkbox" checked={useSaved()} onChange={e=>setUseSaved(e.currentTarget.checked)}/>{tr("Use the saved sign-in where a machine already has one (%1 of %2)", savedCount(), picked().length)}</label></Show>
     <label class="check"><input type="radio" name="atm-how" checked={method()==="copy"} disabled={!view.sources.length} onChange={()=>setMethod("copy")}/>{tr("Copy the existing sign-in over SSH from")}<select class="field atm-src" value={source()} disabled={method()!=="copy"||!view.sources.length} onChange={e=>setSource(e.currentTarget.value)}><For each={view.sources}>{s=><option value={s.key}>{s.label}</option>}</For></select></label>
     <label class="check"><input type="radio" name="atm-how" checked={method()==="signin"} onChange={()=>setMethod("signin")}/>{tr("Create the profile and sign in on each machine (a terminal opens there)")}</label>
    </div>}><div class="form-row"><label for="atm-key">{tr("DeepSeek API key")}</label><input id="atm-key" class="field" type="password" autocomplete="off" maxLength={4096} value={apiKey()} onInput={e=>setApiKey(e.currentTarget.value)}/></div><p class="hint">{tr("The same key is saved on every selected machine.")}</p></Show>
    <div class="form-row"><label for="atm-name">{tr("Account name")}</label><input id="atm-name" class="field" maxLength={160} value={name()} onInput={e=>setName(e.currentTarget.value)}/></div>
    <div class="form-row"><label for="atm-id">{tr("Profile ID")}</label><input id="atm-id" class="field mono" maxLength={48} disabled={view.provider==="dsh"} classList={{invalid:!idValid()}} value={profileId()} onInput={e=>setProfileId(e.currentTarget.value)}/></div>
    <Show when={!idValid()}><p class="hint warn">{tr("Use lowercase letters, digits and hyphens, starting with a letter or digit.")}</p></Show>
   </Show>
   <Show when={step()===3}>
    <p class="atm-summary"><Show when={finished()} fallback={<>{state.progress().length===1?tr("Adding %1 to 1 machine…",view.label):tr("Adding %1 to %2 machines…",view.label,state.progress().length)}</>}>{[tr("%1 added",count("added")),count("needs-signin")?tr("%1 need sign-in",count("needs-signin")):"",count("failed")?tr("%1 failed",count("failed")):"",count("unknown")?tr("%1 unknown",count("unknown")):""].filter(Boolean).join(", ")}.</Show></p>
    <div class="atm-results"><For each={ordered()}>{p=><div class={`atm-result r-${p.state}`}><span class="atm-ricon" aria-hidden="true"><Show when={p.state==="working"} fallback={<Icon name={p.state==="added"?"check":p.state==="needs-signin"?"key-round":p.state==="failed"?"x":p.state==="unknown"?"circle-alert":"clock"} size={14}/>}><i class="atm-spin"/></Show></span><MachineLabel alias={p.machine}/><span class="atm-rtext">{p.text}</span><Show when={p.state==="failed"}><button class="btn small" disabled={state.busy()} onClick={()=>void state.retry(p.host,values())}>{tr("Retry")}</button></Show><Show when={p.state==="needs-signin"&&p.profile}><button class="btn small" disabled={state.busy()} onClick={e=>void signInDistributedProfile(p.profile!,e.currentTarget)}>{tr("Sign in…")}</button></Show></div>}</For></div>
    <Show when={count("unknown")}><p class="hint">{tr("An unknown result is never retried automatically: the copy may have finished. Open the machine in Accounts to check it.")}</p></Show>
   </Show>
  </div>
  <div class="dlg-foot"><Show when={step()===1}><span class="dim atm-count">{picked().length?tr("%1 selected",picked().length):tr("Choose one or more machines")}</span></Show><span class="spacer"/><Show when={step()!==3} fallback={<button class="btn primary" disabled={!finished()} onClick={()=>state.done(view.handle)}>{tr("Done")}</button>}><Show when={step()===2} fallback={<button class="btn" onClick={()=>state.close(view.handle)}>{tr("Cancel")}</button>}><button class="btn" onClick={()=>setStep(1)}>{tr("Back")}</button></Show><Show when={step()===1} fallback={<button class="btn primary" disabled={!canAdd()} onClick={start}>{picked().length===1?tr("Add to 1 machine"):tr("Add to %1 machines",picked().length)}</button>}><button class="btn primary" disabled={!picked().length} onClick={()=>setStep(2)}>{tr("Next")}</button></Show></Show></div>
 </dialog>;
}
