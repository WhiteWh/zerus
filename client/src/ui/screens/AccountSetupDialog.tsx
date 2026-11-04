import {For,Show,createSignal,onMount,onCleanup} from "solid-js";
import {Icon,AgentGlyph} from "../lib/icons";
import {agentKind} from "../../store/catalog";
import {providerName} from "../lib/usage";
import type {SetupPrompt} from "../../store/accountSetup";
import type {AccountSetupValues} from "../../bridge";
import { tr } from "../../i18n";

/** Approved SettingsDialogs AddAccountDialog / ApiKeyDialog; Qt saved profiles. */
export default function AccountSetupDialog(props:{prompt:SetupPrompt;busy:boolean;close(handle:string):void;confirm(values:AccountSetupValues,handle:string):void}) {
 const view=props.prompt;
 const first=view.hosts.find(h=>h.key===(view.kind==="key"?view.host:view.preferredHost))??view.hosts.find(h=>h.key==="")??view.hosts[0];
 const [host,setHost]=createSignal(first?.key??""),[provider,setProvider]=createSignal(view.provider),[saved,setSaved]=createSignal(""),[name,setName]=createSignal(""),[api,setApi]=createSignal(view.kind==="key"),[key,setKey]=createSignal("");
 const machine=()=>view.hosts.find(h=>h.key===host());
 const savedFor=(p=provider())=>machine()?.saved.filter(s=>s.provider===p)??[];
 const chooseAgent=(p:string)=>{setProvider(p);setSaved(savedFor(p)[0]?.id??"");};
 const chooseHost=(h:string)=>{setHost(h);const p=machine()?.providers.includes(provider())?provider():machine()?.providers[0]??"codex";chooseAgent(p);};
 chooseHost(host());
 const reuse=()=>!!saved(),ocListed=()=>machine()?.listed.includes("native-opencode"),canAgent=(p:string)=>p!=="opencode"||savedFor(p).length>0;
 const detail=(p:string)=>p==="opencode"?(savedFor(p).length?tr("Add the native configuration again"):ocListed()?tr("Native configuration, already added"):tr("Not installed on %1", machine()?.machine ?? "")):p==="dsh"?(savedFor(p).length?tr("Saved API key or a new one"):tr("DeepSeek API key")):savedFor(p).length?tr("Saved account or a new profile"):tr("New profile with its own sign-in");
 const hint=()=>!machine()?.online?`${machine()?.machine??"Machine"} is offline.`:reuse()&&provider()==="opencode"?tr("Use OpenCode's native configuration on %1 for new sessions again.", machine()?.machine ?? ""):reuse()?tr("Use the saved sign-in on %1. The account will be available for new sessions again.", machine()?.machine ?? ""):provider()==="dsh"?tr("Configure a DeepSeek API key on the selected machine."):tr("Create a separate account profile on the selected machine, then sign in.");
 const can=()=>!!machine()?.online&&(api()?!!key().trim():canAgent(provider())&&(reuse()||!!name().trim()));
 let dialog!:HTMLDialogElement;let keyInput:HTMLInputElement|undefined;
 onMount(()=>{dialog.showModal();queueMicrotask(()=>dialog.querySelector<HTMLElement>(api()?"input":"select")?.focus());});
 onCleanup(()=>{setKey("");dialog.close();if(view.origin.isConnected)view.origin.focus();});
 const submit=(e:Event)=>{e.preventDefault();if(!can()||props.busy)return;if(!api()&&!reuse()&&provider()==="dsh"&&machine()?.deepseek_installed){setApi(true);queueMicrotask(()=>keyInput?.focus());return;}props.confirm({host:host(),provider:provider(),saved:saved(),label:view.kind==="key"?view.label:name().trim(),api_key:key().trim()},view.handle);setKey("");};
 const titleId=()=>api()?"accounts-api-key-title":"accounts-add-title";
 return <dialog ref={dialog} class="dlg" style={{width:api()?"460px":"520px"}} aria-labelledby={titleId()} onCancel={e=>{e.preventDefault();if(!props.busy)props.close(view.handle);}} onClose={()=>props.close(view.handle)}><form onSubmit={submit}>
  <div class="dlg-head"><Icon name={api()?"key-round":"user-round-cog"} size={18} class="accent"/><h2 id={titleId()}>{api()?`DeepSeek API key on ${machine()?.machine}`:tr("Add agent account")}</h2><span class="grow"/><button type="button" class="icon-btn" aria-label={tr("Close")} disabled={props.busy} onClick={()=>props.close(view.handle)}><Icon name="x" size={16}/></button></div>
  <div class="dlg-body"><Show when={api()} fallback={<>
   <div class="form-row"><label for="aa-host">{tr("Machine")}</label><select id="aa-host" class="field" value={host()} onChange={e=>chooseHost(e.currentTarget.value)}><For each={view.hosts}>{h=><option value={h.key}>{h.label}{h.online?"":" (offline)"}</option>}</For></select></div>
   <div class="form-row acct-agents-row"><label>{tr("Agent")}</label><div class="acct-agents" role="listbox" aria-label={tr("Agent")}><For each={machine()?.providers}>{p=><button type="button" role="option" class="acct-agent" classList={{on:provider()===p}} aria-selected={provider()===p} disabled={!canAgent(p)} title={p==="opencode"?(ocListed()?tr("Configure providers, models and permissions with OpenCode on %1. Zerus uses that native configuration.", machine()?.machine ?? ""):tr("Install OpenCode on %1, then refresh Accounts.", machine()?.machine ?? "")):undefined} style={{"--agent":`var(--z-agent-${agentKind(p)})`}} onClick={()=>chooseAgent(p)}><AgentGlyph agent={agentKind(p)} size={16}/><span class="col"><b>{providerName(p)}</b><span class="muted">{detail(p)}</span></span></button>}</For></div></div>
   <div class="form-row"><label for="aa-saved">{tr("Account")}</label><select id="aa-saved" class="field" value={saved()} disabled={savedFor().length+(provider()==="opencode"?0:1)<2} onChange={e=>setSaved(e.currentTarget.value)}><For each={savedFor()}>{s=><option value={s.id}>{s.label}</option>}</For><Show when={provider()!=="opencode"}><option value="">{tr("New account")}</option></Show></select></div>
   <div class="form-row"><label for="aa-name">{tr("Account name")}</label><input id="aa-name" class="field" maxLength={160} placeholder={tr("Work, Personal, Team…")} disabled={reuse()} value={reuse()?"":name()} onInput={e=>setName(e.currentTarget.value)}/></div>
  </>}><p>Save an API key on {machine()?.machine}. DeepSeek on that machine will use this key.</p><div class="form-row"><label for="ak-key">{tr("API key")}</label><input ref={keyInput} disabled={props.busy} id="ak-key" class="field" type="password" maxLength={4096} autocomplete="off" value={key()} onInput={e=>setKey(e.currentTarget.value)}/></div></Show></div>
  <div class="dlg-foot"><Show when={!api()}><span class="hint">{hint()}</span></Show><span class="spacer"/><button type="button" class="btn" disabled={props.busy} onClick={()=>props.close(view.handle)}>{tr("Cancel")}</button><button type="submit" class="btn primary" classList={{"acct-add-btn":!api()}} disabled={!can()||props.busy}>{api()?(props.busy?tr("Saving on %1…", machine()?.machine ?? ""):tr("Save on %1", machine()?.machine ?? "")):reuse()?tr("Add account"):provider()==="dsh"?tr("Enter API key…"):tr("Create and sign in")}</button></div>
 </form></dialog>;
}
