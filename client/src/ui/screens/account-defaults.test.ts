import {describe,it,expect} from "vitest";
import {accountModelChoices,accountModelView,permissionExplanation} from "./account-defaults";
import type {AccountModelCatalog,AccountEditView} from "../../bridge";
const catalog:AccountModelCatalog={models:[{id:"example",label:"Example model",effort_options:["low","high"]}],defaults:{model:"example",effort:"high"},provider_defaults:{model:"native",effort:"low"},effort_supported:true};
describe("account-specific defaults",()=>{
  it("uses the captured account's choices and preserves a missing saved model",()=>{
    expect(accountModelChoices(catalog).map(m=>m.label)).toEqual(["Example model"]);
    const missing={...catalog,defaults:{model:"old",effort:"high"}};
    expect(accountModelChoices(missing).at(-1)?.label).toBe("old (currently unavailable)");
    expect(accountModelView(missing,"old").canSave).toBe(false);expect(accountModelView(missing,"").canSave).toBe(true);
  });
  it("disables inherited and Kimi effort with Qt's hints",()=>{
    expect(accountModelView(catalog,"example")).toMatchObject({efforts:["low","high"],effortEnabled:true,canSave:true});
    expect(accountModelView(catalog,"").effortEnabled).toBe(false);
    expect(accountModelView({...catalog,effort_supported:false},"example")).toMatchObject({efforts:[],effortEnabled:false,hint:"Kimi manages effort in its native settings. This choice sets the model for new conversations."});
    expect(accountModelView({...catalog,models:[]},"").hint).toBe("Open this account's agent once, then refresh to load its models.");
  });
  it("explains the captured provider permission scope",()=>{
    const view={provider:"codex",machine:"studio",permission_mode:"provider",permission_detail:"Configured by the provider."} as AccountEditView;
    expect(permissionExplanation(view,"provider")).toBe("Inherit Codex settings on studio. Configured by the provider.");
    expect(permissionExplanation(view,"bypass")).toBe("Full access without approval prompts or a sandbox.");
    expect(permissionExplanation({...view,provider:"kimi"},"bypass")).toBe("Run tools and answer questions automatically (Never Ask).");
    expect(permissionExplanation({...view,permission_mode:"bypass"},"provider")).not.toContain("Configured by the provider.");
  });
});
