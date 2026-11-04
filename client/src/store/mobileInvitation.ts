import {createSignal} from "solid-js";

export interface ComputerTarget {config_revision:string;server_url:string}
export interface PendingComputerInvitation {handle:string;target:ComputerTarget;expires_at:number}

/** One app-lifetime cancellation handle. No credential, persistence or timer. */
export function createMobileInvitationMemory(){
 const [pending,setPending]=createSignal<PendingComputerInvitation>(),[busy,setBusy]=createSignal(false);
 let target:ComputerTarget|undefined;
 return {pending,busy,setBusy,target:()=>target,observe:(next:ComputerTarget|undefined)=>{target=next;},
  retain(value:PendingComputerInvitation){setPending({handle:value.handle,target:{config_revision:value.target.config_revision,server_url:value.target.server_url},expires_at:value.expires_at});},
  clear(handle?:string){const value=pending();if(value&&(!handle||value.handle===handle)){setPending(undefined);return value;}}
 };
}
export const mobileInvitation=createMobileInvitationMemory();
