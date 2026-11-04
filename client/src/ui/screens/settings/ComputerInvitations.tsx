import {createEffect,onCleanup,onMount} from "solid-js";
import {invoke} from "@tauri-apps/api/core";
import {Icon} from "../../lib/icons";
import {toast} from "../../../store/ui";
import {Actions,Status} from "./ui";
import {createComputerCodes} from "./computer-invitations";
import {mobileInvitation} from "../../../store/mobileInvitation";
import type {Workspace} from "./mobile-page";
import { tr } from "../../../i18n";

/** Mockup Mobile.ws-invite; codes never enter the shared Mobile page. */
export default function ComputerInvitations(props:{workspace:Workspace;busy:boolean;refresh:()=>void}){
 const state=createComputerCodes({
  open:target=>invoke("mobile_computer_open",{target}),
  issue:handle=>invoke("mobile_computer_invite",{handle}),
  close:handle=>invoke("mobile_computer_close",{handle}),
  cancel:handle=>invoke("mobile_computer_cancel",{handle}),
  check:handle=>invoke("mobile_computer_check",{handle}),
  copy:text=>invoke("copy_text",{text}),refresh:props.refresh,
 },undefined,mobileInvitation);
 createEffect(()=>state.observe(props.workspace));
 onMount(()=>{let ticks=0;const timer=setInterval(()=>{state.tick();if(++ticks%5===0&&!props.busy)void state.check();},1000);onCleanup(()=>clearInterval(timer));});
 onCleanup(()=>state.dispose());
 const busy=()=>props.busy||state.busy();
 return <div class="ws-invite"><Actions>
  <button class="btn" disabled={busy()||!!state.pending()||!!props.workspace.pending} onClick={()=>void state.issue()}><Icon name="key-round" size={13}/>{tr("Give computer code")}</button>
  <button class="btn" disabled={busy()||!state.code()} onClick={()=>void state.copy().then(copied=>{if(copied)toast("Copied",tr("Computer code copied. It stays a credential until used or expired."));})}><Icon name="copy" size={13}/>{tr("Copy computer code")}</button>
  <button class="btn" disabled={busy()||!state.pending()} onClick={()=>void state.cancel()}>{tr("Cancel code")}</button>
 </Actions><Status kind={state.pending()?"ok":"muted"}>{state.status()}</Status></div>;
}
