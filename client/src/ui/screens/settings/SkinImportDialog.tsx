import {Show,createMemo,createSignal,onMount,onCleanup} from "solid-js";
import {Icon} from "../../lib/icons";
import {SkinPreview,variantSummary} from "./Appearance";
import {SAMPLE,MAX_SKIN_BYTES,validateSkin} from "../../../skin/import";
import {importedFiles,importSkin,loadUserSkins} from "../../../skin/user";
import {chooseSkin} from "../../../skin/runtime";
import {toast} from "../../../store/ui";
import { tr } from "../../../i18n";
export default function SkinImportDialog(props:{origin:HTMLElement;close():void}) {
 const origin=props.origin;
 const [text,setText]=createSignal(SAMPLE),[fileName,setFileName]=createSignal(""),[busy,setBusy]=createSignal(false),[error,setError]=createSignal("");
 const result=createMemo(()=>validateSkin(text(),importedFiles()));
 const valid=()=>{const value=result();return value.ok?value:undefined;};
 let picker!:HTMLInputElement,dialog!:HTMLDialogElement,alive=true;
 onMount(()=>{dialog.showModal();void loadUserSkins();});
 onCleanup(()=>{alive=false;dialog.close();if(origin.isConnected)origin.focus();});
 const add=async()=>{if(busy()||!valid())return;setBusy(true);setError("");try{const result=await importSkin(text());if(!alive)return;const applied=chooseSkin(result.skin.id);props.close();toast(tr("Skin imported"),applied?`${result.skin.name} is in use. It is saved in the skins folder on this device.`:`${result.skin.name} is used with the ${result.skin.appearance} appearance.`);}catch(error){if(alive)setError(String((error as Error).message??error));}finally{if(alive)setBusy(false);}};
 return <dialog ref={dialog} class="dlg" style={{width:"760px"}} aria-labelledby="skin-import-title" onCancel={event=>{event.preventDefault();props.close();}} onClose={()=>{if(alive)props.close();}}>
  <div class="dlg-head"><Icon name="palette" size={18} class="accent"/><h2 id="skin-import-title">{tr("Import skin")}</h2><span class="grow"/><button class="icon-btn" aria-label={tr("Close")} onClick={props.close}><Icon name="x" size={16}/></button></div>
  <div class="dlg-body"><div class="si-grid"><div class="col" style={{gap:"8px"}}><div class="row"><button class="btn" onClick={()=>picker.click()}><Icon name="folder-open" size={14}/>{tr("Choose file…")}</button><span class="muted ell">{fileName()||tr("Or paste the skin JSON below")}</span><input ref={picker} type="file" accept=".json,application/json" hidden onChange={async event=>{const file=event.currentTarget.files?.[0];if(!file)return;setFileName(file.name);setError("");if(file.size>MAX_SKIN_BYTES){setText("{}");setError("A skin file must be at most 256 KiB.");return;}try{const value=await file.text();if(alive)setText(value);}catch(error){if(alive)setError(String(error));}}}/></div><textarea class="field mono si-text" spellcheck={false} aria-label={tr("Skin JSON")} maxLength={MAX_SKIN_BYTES} value={text()} onInput={event=>{setText(event.currentTarget.value);setError("");}}/></div>
   <div class="si-result"><Show when={!error()&&valid()} fallback={<div class="notice danger" role="alert"><Icon name="circle-alert" size={14}/><span>{error()||(result() as {error:string}).error}</span></div>}>{r=><><SkinPreview skin={r().skin}/><div class="si-name"><b>{r().skin.name}</b><span class="chip">{r().skin.appearance==="dark"?"Dark":"Light"}</span></div><div class="kv"><span class="muted">{tr("Id")}</span><span class="mono">{r().skin.id}</span></div><div class="kv"><span class="muted">{tr("Based on")}</span><span>{r().file.extends??"zerus-dark (base)"}</span></div><div class="kv"><span class="muted">{tr("Tokens changed")}</span><span class="tabular">{r().tokens}</span></div><p class="muted">{variantSummary(r().skin)}</p><div class="notice"><Icon name="check" size={14}/><span>{tr("Valid skin. Every value matched its token type.")}</span></div></>}</Show></div>
  </div></div>
  <div class="dlg-foot"><span class="hint">{tr("Schema 1: tokens, component variants and layout variants. CSS, selectors, URLs and code are rejected.")}</span><span class="spacer"/><button class="btn" onClick={props.close}>{tr("Cancel")}</button><button class="btn primary" disabled={busy()||!valid()||!!error()} onClick={()=>void add()}>{tr("Add skin")}</button></div>
 </dialog>;
}
