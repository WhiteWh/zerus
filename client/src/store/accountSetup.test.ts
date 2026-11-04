import {describe,it,expect,vi} from "vitest";
import {createAccountSetup} from "./accountSetup";
import type {AccountSetupKind,AccountSetupView} from "../bridge";
const origin={} as HTMLElement;
function fixture() {
 let next=0;
 const open=vi.fn(async(kind:AccountSetupKind):Promise<AccountSetupView>=>({handle:`setup-${++next}`,kind,hosts:[],host:"studio",provider:"codex",label:"Example"}));
 const apply=vi.fn(async()=>({notices:["Account added on studio using its saved sign-in."],errors:[] as string[],profile:"studio\u001fexample" as string|null}));
 const close=vi.fn(async()=>{}),error=vi.fn(),select=vi.fn();
 return {open,apply,close,error,select,state:createAccountSetup({open,apply,close,error,select})};
}
describe("account setup ownership",()=>{
 it("cancels an open Add without creating anything",async()=>{const t=fixture();await t.state.open("add",origin,"","","studio");expect(t.state.prompt()?.preferredHost).toBe("studio");t.state.close();expect(t.close).toHaveBeenCalledWith("setup-1");expect(t.apply).not.toHaveBeenCalled();});
 it("keeps the selected machine and saved profile under a single handle",async()=>{const t=fixture();await t.state.open("add",origin);await Promise.all([t.state.confirm({host:"studio",provider:"codex",saved:"old"},"setup-1"),t.state.confirm({host:"studio"},"setup-1")]);expect(t.apply).toHaveBeenCalledExactlyOnceWith("setup-1",{host:"studio",provider:"codex",saved:"old"});expect(t.select).toHaveBeenCalledWith("studio\u001fexample");});
 it("does not replay uncertain creation and clears the submit payload",async()=>{const t=fixture();t.apply.mockRejectedValueOnce(new Error("Unknown result"));await t.state.open("add",origin);await t.state.confirm({host:"studio",provider:"dsh",api_key:"fixture-secret"},"setup-1");expect(t.state.prompt()).toBeUndefined();await t.state.confirm({host:"studio"},"setup-1");expect(t.apply).toHaveBeenCalledTimes(1);expect(t.error).toHaveBeenCalledWith("Error: Unknown result");});
 it("an old dialog cannot close or submit a newer one",async()=>{const t=fixture();await t.state.open("add",origin);t.state.close();await t.state.open("key",origin,"group","peer");t.state.close("setup-1");await t.state.confirm({api_key:"fixture"},"setup-1");expect(t.state.prompt()?.handle).toBe("setup-2");expect(t.apply).not.toHaveBeenCalled();});
 it("releases a late snapshot after navigation",async()=>{const t=fixture();let resolve!:(v:AccountSetupView)=>void;t.open.mockImplementationOnce(()=>new Promise(r=>resolve=r));const pending=t.state.open("add",origin);t.state.close();resolve({handle:"late",kind:"add",hosts:[],host:"",provider:"codex",label:""});await pending;expect(t.close).toHaveBeenCalledWith("late");expect(t.state.prompt()).toBeUndefined();expect(t.apply).not.toHaveBeenCalled();});
 it("interactive actions dispatch their exact native snapshot once",async()=>{for(const kind of ["login","install","start"] as const){const t=fixture();await t.state.open(kind,origin,"group","studio-profile");expect(t.open).toHaveBeenCalledWith(kind,"group","studio-profile");expect(t.apply).toHaveBeenCalledExactlyOnceWith("setup-1",{});expect(t.state.prompt()).toBeUndefined();}});
 it("keeps a confirmed created profile when its terminal failed",async()=>{const t=fixture();t.apply.mockResolvedValueOnce({notices:[],errors:["Could not open agent sign-in"],profile:"studio\u001fcreated"});await t.state.open("add",origin);await t.state.confirm({host:"studio",provider:"codex",label:"Work"},"setup-1");expect(t.select).toHaveBeenCalledWith("studio\u001fcreated");expect(t.error).toHaveBeenCalledWith("Could not open agent sign-in");expect(t.apply).toHaveBeenCalledTimes(1);});
});
