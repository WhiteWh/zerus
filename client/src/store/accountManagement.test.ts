import { describe, it, expect, vi } from "vitest";
import { createAccountManagement } from "./accountManagement";
import type { AccountEditKind, AccountEditView, AccountEditValues } from "../bridge";

const origin = {} as HTMLElement;
function setup() {
  const open=vi.fn(async (_account:string,_profile:string,kind:AccountEditKind):Promise<AccountEditView>=>({handle:"native-snapshot",kind,label:"Example",machine:"studio",provider:"codex"}));
  const apply=vi.fn(async (_handle:string,_values:AccountEditValues)=>({notices:["Account name updated."],errors:[] as string[]}));
  const close=vi.fn(async (_handle:string)=>{}), notice=vi.fn();
  return { open,apply,close,notice, state:createAccountManagement({open,apply,close,notice}) };
}
describe("captured account catalog actions",()=>{
  it("an old dialog close or submit cannot affect a newer snapshot",async()=>{
    const t=setup();let next=0;t.open.mockImplementation(async(_a,_p,kind)=>({handle:`handle-${++next}`,kind,label:"Example",machine:"studio",provider:"codex"}));
    await t.state.open("remove","group","peer",origin);t.state.close("handle-1");
    await t.state.open("rename","group","peer",origin);t.state.close("handle-1");await t.state.confirm("Updated","handle-1");
    expect(t.state.prompt()?.handle).toBe("handle-2");expect(t.apply).not.toHaveBeenCalled();
    await t.state.confirm("Updated","handle-2");expect(t.apply).toHaveBeenCalledExactlyOnceWith("handle-2",{label:"Updated"});
  });
  it("saves permission values only through the original handle and skips an unchanged mode",async()=>{
    const t=setup();t.open.mockImplementation(async (_a,_p,kind)=>({handle:"permissions",kind,label:"Example",machine:"studio",provider:"codex",permission_mode:"provider"}));
    await t.state.open("permissions","group","peer",origin);await t.state.confirm({mode:"provider"});expect(t.apply).not.toHaveBeenCalled();
    await t.state.open("permissions","group","peer",origin);await t.state.confirm({mode:"bypass"});expect(t.apply).toHaveBeenCalledExactlyOnceWith("permissions",{mode:"bypass"});
  });
  it("keeps model and effort from the native snapshot and never resubmits them",async()=>{
    const t=setup();t.open.mockImplementation(async (_a,_p,kind)=>({handle:"model",kind,label:"Example",machine:"studio",provider:"codex",models:{models:[],defaults:{model:"old",effort:"high"},provider_defaults:{model:"",effort:""},effort_supported:true}}));
    await t.state.open("model","group","peer",origin);await t.state.confirm({model:"old",effort:"high"});expect(t.apply).not.toHaveBeenCalled();
    await t.state.open("model","group","peer",origin);await t.state.confirm({model:"new",effort:"low"});await t.state.confirm({model:"new",effort:"low"});expect(t.apply).toHaveBeenCalledExactlyOnceWith("model",{model:"new",effort:"low"});
  });
  it("applies the captured handle once and never retargets by the later selection",async()=>{
    const t=setup();await t.state.open("rename","account-group","studio-profile",origin);
    expect(t.open).toHaveBeenCalledWith("account-group","studio-profile","rename");
    expect(t.state.prompt()?.origin).toBe(origin);
    await Promise.all([t.state.confirm("New label"),t.state.confirm("New label")]);
    expect(t.apply).toHaveBeenCalledExactlyOnceWith("native-snapshot",{label:"New label"});
    expect(t.state.prompt()).toBeUndefined();expect(t.notice).toHaveBeenCalledWith(false,"Account name updated.");
    expect(t.state.notice()).toBe("Account name updated.");t.state.clearNotice();expect(t.state.notice()).toBe("");
  });
  it("cancels removal and releases its native snapshot without applying",async()=>{
    const t=setup();await t.state.open("remove","group","peer-profile",origin);t.state.close();
    expect(t.apply).not.toHaveBeenCalled();expect(t.close).toHaveBeenCalledWith("native-snapshot");
  });
  it("closes an unchanged rename without a catalog write",async()=>{
    const t=setup();await t.state.open("rename","group","profile",origin);await t.state.confirm(" Example ");
    expect(t.apply).not.toHaveBeenCalled();expect(t.close).toHaveBeenCalledWith("native-snapshot");
  });
  it("makes the chosen profile default directly with one captured request",async()=>{
    const t=setup();await t.state.open("default","group","peer-profile",origin);
    expect(t.state.prompt()).toBeUndefined();expect(t.apply).toHaveBeenCalledExactlyOnceWith("native-snapshot",{});
  });
  it("reports uncertain or partial errors without replaying",async()=>{
    const t=setup();t.apply.mockRejectedValueOnce(new Error("Delivery uncertain"));
    await t.state.open("remove","group","profile",origin);await t.state.confirm();await t.state.confirm();
    expect(t.apply).toHaveBeenCalledTimes(1);expect(t.notice).toHaveBeenCalledWith(true,"Error: Delivery uncertain");expect(t.state.busy()).toBe(false);
    t.apply.mockResolvedValueOnce({notices:["Account name updated."],errors:["studio: Accounts changed"]});
    await t.state.open("rename","group","profile",origin);await t.state.confirm("Another");
    expect(t.notice).toHaveBeenCalledWith(true,"studio: Accounts changed");
  });
  it("discards a late open response after its screen closes",async()=>{
    const t=setup();let resolve!:(v:AccountEditView)=>void;
    t.open.mockImplementationOnce(()=>new Promise(r=>{resolve=r;}));
    const pending=t.state.open("remove","group","profile",origin);t.state.close();
    resolve({handle:"late",kind:"remove",label:"Example",machine:"studio",provider:"codex"});await pending;
    expect(t.state.prompt()).toBeUndefined();expect(t.close).toHaveBeenCalledWith("late");expect(t.apply).not.toHaveBeenCalled();
  });
});
