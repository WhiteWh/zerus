// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import type { NewSessionState } from "../../store/newSession";

// Real dialog/TextInput; edits publish a model echo and an asynchronous form
// answer, without a native bridge or any session creation.
const fixture = vi.hoisted(() => ({
  read: () => undefined as NewSessionState | undefined,
  write: (_state: NewSessionState | undefined) => {},
  refresh: () => {},
}));
// Task #433: this file covers the classic layout; the skin now defaults to the dense view.
vi.mock("../../skin/runtime", () => ({ skin: () => ({ layout: { "new-session": "classic" }, appearance: "dark" }) }));
vi.mock("../../store/newSession", async () => {
  const { createSignal } = await import("solid-js");
  const [state, setState] = createSignal<NewSessionState>();
  fixture.read = state;
  fixture.write = (value) => { setState(value); };
  fixture.refresh = () => setState((s) => s && ({ ...s, form: s.form && { ...s.form } }));
  const update = (patch: Partial<NewSessionState>) => {
    setState((s) => s && ({ ...s, ...patch }));
    queueMicrotask(fixture.refresh);
  };
  return {
    newSession: state, closeNewSession: () => setState(undefined), startNewSession: vi.fn(),
    newSessionActions: {
      chooseProject: vi.fn((project: number) => update({ project })),
      setName: (name: string) => update({ name }), setUrl: (url: string) => update({ url }),
      setDestination: (destination: string) => update({ destination }),
      setNewProjectName: (newProjectName: string) => update({ newProjectName }),
      setSource: (repository: boolean) => {
        const s = state();
        if (!s || s.repository === repository) return;
        update({ repository, newProjectVisible: repository, newProjectRow: repository,
          focusUrl: s.focusUrl + Number(repository) });
      },
    },
  };
});
import { newSessionActions } from "../../store/newSession";
import { NewSessionDialog } from "./NewSessionDialog";

let dispose: (() => void) | undefined;
const settle = async () => { for (let i = 0; i < 4; i++) await Promise.resolve(); };
const input = (id: string) => document.getElementById(id) as HTMLInputElement;
function state(): NewSessionState {
  return {
    projects: [{ id: "fixture", name: "Fixture", color: null, palette: 0, newProject: false }], project: 0,
    newProjectVisible: false, newProjectRow: false, newProjectName: "Fixture clone",
    computers: [{ target: "", label: "Fixture PC", online: true }], computer: 0, host: "", style: "posix", noMachine: false,
    repository: false, url: "https://example.invalid/team/project.git", destination: "/fixture/clone", repositoryParent: "/fixture", focusUrl: 0,
    folders: [{ key: "fixture", path: "/fixture/project", name: "Project", tooltip: "/fixture/project" }], folder: 0, folderPath: "/fixture/project", autoPreferred: true,
    worktrees: { catalog: {}, automatic: true, chooseVisible: false, createVisible: false }, accounts: [], account: -1, accountStatus: "", refreshEnabled: true,
    agent: "codex", accountId: "", name: "fixture-session", branch: "fixture", openTerminal: false,
    form: { fields_enabled: true, cancel_enabled: true, start_enabled: false, start_text: { source: "Start session" }, automatic: true,
      auto_enabled: true, none_enabled: true, worktree_options_enabled: false, browse_enabled: true, open_terminal_enabled: true,
      open_terminal_tooltip: { source: "" }, notice: null, name_problem: null, preview: null, outside_project: false,
      worktree_target: "", worktree_tooltip: null, choose_existing_enabled: false, create_manually_enabled: false, clone_launch_due: false },
    error: "", starting: false, submitted: false, models: [], model: "", modelVisible: false, modelEnabled: false, modelButton: "", pickerOpen: false,
    picker: { groups: [], notice: "", all: { checked: false, enabled: false, tooltip: "" }, selected: undefined, query: "", favoritesAvailable: false },
    effortVisible: false, effort: "", efforts: [], service: { visible: false, reason: "", offer: false, text: "", enabled: false, tooltip: "" },
  };
}
beforeEach(async () => {
  vi.mocked(newSessionActions.chooseProject).mockClear();
  fixture.write(state());
  vi.spyOn(HTMLDialogElement.prototype, "showModal").mockImplementation(function (this: HTMLDialogElement) { this.open = true; });
  vi.spyOn(HTMLDialogElement.prototype, "close").mockImplementation(function (this: HTMLDialogElement) { this.open = false; });
  const host = document.createElement("div"); document.body.append(host);
  dispose = render(() => createComponent(NewSessionDialog, {}), host);
  await settle();
});

describe("explicit Destination choices", () => {
  const button = () => document.querySelector<HTMLButtonElement>('[aria-controls="ns-project-list"]')!;
  const options = () => document.querySelectorAll<HTMLElement>("#ns-project-list [role=option]");
  const destinations = async () => {
    const s = fixture.read()!;
    fixture.write({ ...s, projects: [
      { id: "ungrouped", name: "General", color: null, palette: 0, newProject: false },
      { id: "zerus", name: "Zerus", color: null, palette: 1, newProject: false },
    ], project: 0 });
    await settle();
  };

  it.each(["pointer", "open keyboard", "closed keyboard"])("records reselecting renamed Inbox by %s", async (method) => {
    await destinations();
    if (method === "closed keyboard") {
      button().dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true }));
    } else {
      button().click(); await settle();
      expect(options()[0].textContent).toContain("General");
      expect(options()[0].getAttribute("aria-selected")).toBe("true");
      if (method === "pointer") options()[0].click();
      else button().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    }
    await settle();
    expect(newSessionActions.chooseProject).toHaveBeenCalledExactlyOnceWith(0);
    expect(fixture.read()!.project).toBe(0);
    expect(button().getAttribute("aria-expanded")).toBe("false");
  });

  it("records choosing another project", async () => {
    await destinations();
    button().click(); await settle();
    options()[1].click(); await settle();
    expect(newSessionActions.chooseProject).toHaveBeenCalledExactlyOnceWith(1);
    expect(fixture.read()!.project).toBe(1);
  });

  it("keeps the default untouched when the list is only opened and dismissed", async () => {
    await destinations();
    button().click(); await settle();
    fixture.refresh(); await settle();
    button().click(); await settle();
    expect(newSessionActions.chooseProject).not.toHaveBeenCalled();
    expect(fixture.read()!.project).toBe(0);
  });
});
afterEach(() => { dispose?.(); dispose = undefined; fixture.write(undefined); document.body.replaceChildren(); vi.restoreAllMocks(); });

async function type(id: string, key: "name" | "url" | "destination" | "newProjectName") {
  const field = input(id), before = field.value;
  field.focus();
  for (const char of "abc") {
    field.value += char;
    field.setSelectionRange(field.value.length, field.value.length);
    field.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: char }));
    await settle();
    expect(document.activeElement, id).toBe(field);
    expect(input(id), "typing keeps the original editor").toBe(field);
    expect(field.disabled).toBe(false);
    fixture.refresh(); await settle();
    expect(document.activeElement, `${id} after form refresh`).toBe(field);
  }
  expect(field.value).toBe(before + "abc");
  expect(fixture.read()![key]).toBe(before + "abc");
}

describe("New session text focus", () => {
  it.each([
    ["ns-name", "name"], ["ns-url", "url"], ["ns-destination", "destination"], ["ns-new-project", "newProjectName"],
  ] as const)("keeps typing in %s through repository-form answers", async (id, key) => {
    newSessionActions.setSource(true); await settle();
    expect(document.activeElement).toBe(input("ns-url"));
    expect(fixture.read()!.focusUrl).toBe(1);
    await type(id, key);
    expect(fixture.read()!.focusUrl).toBe(1);
  });

  it("keeps folder-source name focus and focuses URL once for each explicit repository transition", async () => {
    await type("ns-name", "name");
    newSessionActions.setSource(true); await settle();
    expect(document.activeElement).toBe(input("ns-url"));
    input("ns-name").focus();
    newSessionActions.setSource(true); fixture.refresh(); await settle();
    expect(document.activeElement).toBe(input("ns-name"));
    newSessionActions.setSource(false); await settle();
    await type("ns-name", "name");
    newSessionActions.setSource(true); await settle();
    expect(fixture.read()!.focusUrl).toBe(2);
    expect(document.activeElement).toBe(input("ns-url"));
  });

  it("keeps the core's field lock enforced", async () => {
    newSessionActions.setSource(true); await settle();
    const s = fixture.read()!;
    fixture.write({ ...s, form: { ...s.form!, fields_enabled: false } }); await settle();
    for (const id of ["ns-name", "ns-url", "ns-destination", "ns-new-project"]) expect(input(id).disabled).toBe(true);
  });
});
