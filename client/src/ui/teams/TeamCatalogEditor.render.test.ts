// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { createComponent, createRoot } from "solid-js";
import { render } from "solid-js/web";
import type { AccountsView, CatalogEdit, CatalogEditorView, CatalogSchema } from "../../types";
import { createCatalogEditor, type CatalogEditorPort } from "../../store/catalogEditor";
import { TeamCatalogEditor } from "./TeamCatalogEditor";

vi.mock("../../store/accounts", () => {
  const profile = { key: "work", host: "", machine: "workstation", id: "work", label: "Work", provider: "kimi",
    native: false, installed: true, is_default: false, signed_in: true, usage: { identity: {} } };
  const account = { key: "work", label: "Work", provider: "kimi", installed: true, machines: ["workstation"], members: [profile], usage: { identity: {} } };
  return { accountsPage: () => ({ accounts: [account] }) as unknown as AccountsView, loadAccounts: vi.fn() };
});

const schema: CatalogSchema = {
  roles: [
    { name: "account", label: "Account", hint: "", type: "text" },
    { name: "model", label: "Default model", hint: "", type: "text" },
    { name: "effort", label: "Effort", hint: "", type: "text" },
  ],
  templates: [], routing: [], pipelines: [],
};
const kimis = [
  { id: "kimi-code/kimi-for-coding", label: "K2.8 Preview", effort_options: ["low", "high", "max"], default_effort: "max" },
  { id: "kimi-code/kimi-for-coding-highspeed", label: "K2.7 Code Highspeed", effort_options: ["on"], default_effort: "on" },
  { id: "kimi-code/k3", label: "K3", effort_options: ["low", "high", "max"], default_effort: "high" },
  { id: "kimi-code/k3-256k", label: "K3-256k", effort_options: ["low", "high", "max"], default_effort: "high" },
  { id: "kimi-code/k3.1-pro", label: "K3.1 Pro", effort_options: ["low", "high", "max"], default_effort: "high" },
  { id: "kimi-code/k3.1-pro-highspeed", label: "K3.1 Pro Highspeed 2x Usage Cost", effort_options: ["low", "high", "max"], default_effort: "high" },
];
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
let host: HTMLDivElement | undefined, dispose: (() => void) | undefined, editorDispose: (() => void) | undefined;
afterEach(() => { dispose?.(); editorDispose?.(); host?.remove(); vi.clearAllMocks(); });

function field(label: string): HTMLElement {
  const found = [...host!.querySelectorAll<HTMLElement>(".cat-field")].find((item) => item.querySelector("label")?.textContent === label)!;
  return found;
}
function combobox(label: string): HTMLButtonElement {
  return field(label).querySelector<HTMLButtonElement>('[role="combobox"]')!;
}
async function choose(label: string, value: string) {
  combobox(label).click(); await flush();
  host!.querySelector<HTMLButtonElement>(`[role="option"][data-value="${value}"]`)!.click(); await flush();
}

describe("agent provider fields", () => {
  it("uses Dropdowns, fills the default account and offers every Kimi model", async () => {
    const edits: CatalogEdit[] = [];
    let role: Record<string, unknown> = { id: "coder", name: "Coder", computer: "", agent: "kimi", account: "", model: "", effort: "" };
    const view = (): CatalogEditorView => ({ version: 1, builtin: false, roles: [{ ...role }], templates: [], routing: [], pipelines: [], planning_rules: "", unsaved: 0, edited: {} });
    const port = {
      schema: vi.fn(async () => schema),
      load: vi.fn(async () => view()),
      edit: vi.fn(async (_computer: string, change: CatalogEdit) => {
        edits.push(change);
        if (change.op === "set" && change.kind === "role") role = { ...role, [change.field]: change.value };
        return view();
      }),
    } as unknown as CatalogEditorPort;
    const editor = createRoot((dispose) => { editorDispose = dispose; return createCatalogEditor(port); });
    await editor.load();
    host = document.createElement("div"); document.body.append(host);
    dispose = render(() => createComponent(TeamCatalogEditor, { editor, models: vi.fn(async () => kimis) }), host);
    await flush();

    expect(combobox("Account").textContent).toContain("Default account");
    expect(combobox("Default model").textContent).toContain("default model");
    expect(field("Effort").querySelector("select")).toBeNull();
    expect(field("Default model").querySelector("input")).toBeNull();

    await choose("Account", "work");
    expect(edits).toEqual([
      { op: "set", kind: "role", id: "coder", field: "account", value: "work" },
      { op: "set", kind: "role", id: "coder", field: "model", value: "" },
      { op: "set", kind: "role", id: "coder", field: "effort", value: "" },
    ]);

    combobox("Default model").click(); await flush();
    const options = [...field("Default model").querySelectorAll<HTMLElement>('[role="option"]')];
    expect(options).toHaveLength(8);
    expect(options.map((option) => option.dataset.value)).toEqual(["", ...kimis.map((model) => model.id), "__custom_model__"]);
    host!.querySelector<HTMLButtonElement>('[role="option"][data-value="kimi-code/k3.1-pro"]')!.click(); await flush();
    expect(edits.at(-2)).toEqual({ op: "set", kind: "role", id: "coder", field: "model", value: "kimi-code/k3.1-pro" });
    expect(edits.at(-1)).toEqual({ op: "set", kind: "role", id: "coder", field: "effort", value: "high" });

    combobox("Effort").click(); await flush();
    expect([...field("Effort").querySelectorAll<HTMLElement>('[role="option"]')].map((option) => option.dataset.value)).toEqual(["", "low", "high", "max"]);

    await choose("Default model", "__custom_model__");
    const input = field("Default model").querySelector<HTMLInputElement>("input")!;
    input.value = "custom/kimi";
    input.dispatchEvent(new Event("change", { bubbles: true })); await flush();
    expect(edits.at(-2)).toEqual({ op: "set", kind: "role", id: "coder", field: "model", value: "custom/kimi" });
    expect(edits.at(-1)).toEqual({ op: "set", kind: "role", id: "coder", field: "effort", value: "" });
  });
});
