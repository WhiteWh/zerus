import { createRoot } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import { emptyGrouping } from "../../store/organization";
import type { EditOutcome, Project } from "../../types";
import { catalogName, createCatalogEdits } from "./project-catalog";

const project = (id: string, name = id): Project => ({ id, name, accessible: true, is_default: false, palette: 0, color: null, folders: [] });
const origin = {} as HTMLElement;
function setup() {
  let projects = [project("a"), project("b"), project("c")];
  let grouping = { ...emptyGrouping(), ready: true, default_project: "b" };
  const done = (): Promise<EditOutcome> => Promise.resolve({ outcome: "done", conflicts: 0 });
  const api = { projects: () => projects, grouping: () => grouping,
    createProject: vi.fn(async (): Promise<EditOutcome> => ({ outcome: "done", conflicts: 0, project: "new-exact-id" })),
    renameProject: vi.fn(done), deleteProject: vi.fn(done), moveProject: vi.fn(done), select: vi.fn() };
  let dispose!: () => void;
  const edits = createRoot((stop) => { dispose = stop; return createCatalogEdits(api); });
  return { api, edits, dispose, projects: (next: Project[]) => { projects = next; }, ready: (ready: boolean) => { grouping = { ...grouping, ready }; } };
}

describe("Projects catalog controls", () => {
  it("creates once and selects the reserved identity even when names collide", async () => {
    const { api, edits, projects, dispose } = setup();
    projects([project("somebody-else", "Same name")]);
    edits.openNew(origin);
    await edits.submitNew("  Same name  ");
    await edits.submitNew("Same name");
    expect(api.createProject).toHaveBeenCalledExactlyOnceWith("Same name");
    expect(api.select).toHaveBeenCalledExactlyOnceWith("new-exact-id");
    expect(edits.prompt()).toBeUndefined();
    dispose();
  });
  it("keeps blank or cancelled creation inert, and refuses after readiness disappears", async () => {
    const { api, edits, ready, dispose } = setup();
    edits.openNew(origin); await edits.submitNew(" \n ");
    expect(edits.prompt()?.kind).toBe("new");
    edits.close(); await edits.submitNew("cancelled");
    edits.openNew(origin); ready(false); await edits.submitNew("offline");
    expect(api.createProject).not.toHaveBeenCalled();
    dispose();
  });
  it("selects a kept edit's original ID and never replays a refused or uncertain call", async () => {
    const { api, edits, dispose } = setup();
    api.createProject.mockResolvedValueOnce({ outcome: "kept", detail: "offline", project: "reserved" });
    edits.openNew(origin); await edits.submitNew("new");
    expect(api.select).toHaveBeenLastCalledWith("reserved");
    api.select.mockClear();
    api.createProject.mockResolvedValueOnce({ outcome: "refused", detail: "catalog changed", project: "refused-id" });
    edits.openNew(origin); await edits.submitNew("new");
    await edits.submitNew("new");
    expect(api.createProject).toHaveBeenCalledTimes(2);
    expect(api.select).not.toHaveBeenCalled();
    dispose();
  });
  it("does not steal selection after the screen left while a create was pending", async () => {
    const { api, edits, dispose } = setup();
    let finish!: (value: EditOutcome) => void;
    api.createProject.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    edits.openNew(origin); const pending = edits.submitNew("new");
    expect(edits.busy()).toBe(true);
    edits.openNew(origin); await edits.submitNew("again");
    dispose(); finish({ outcome: "done", conflicts: 0, project: "reserved" }); await pending;
    expect(api.createProject).toHaveBeenCalledTimes(1);
    expect(api.select).not.toHaveBeenCalled();
  });
  it("captures the delete identity, prefers the default receiver and requires explicit confirmation", async () => {
    const { api, edits, dispose } = setup();
    edits.openDelete("a", origin);
    expect(edits.prompt()).toMatchObject({ kind: "delete", project: "a", selected: "b", text: "Delete “a”? Choose the project that will receive its sessions. Files and running agents stay in place." });
    expect(api.deleteProject).not.toHaveBeenCalled();
    await edits.confirmDelete("b"); await edits.confirmDelete("b");
    expect(api.deleteProject).toHaveBeenCalledExactlyOnceWith("a", "b");
    expect(api.select).toHaveBeenCalledExactlyOnceWith("b");
    dispose();
  });
  it("does not delete into itself or a newly introduced, removed or inaccessible receiver", async () => {
    const { api, edits, projects, dispose } = setup();
    edits.openDelete("a", origin);
    await edits.confirmDelete("a");
    projects([project("a"), { ...project("b"), accessible: false }, project("d")]);
    await edits.confirmDelete("b"); await edits.confirmDelete("d");
    projects([project("b")]); await edits.confirmDelete("b");
    expect(api.deleteProject).not.toHaveBeenCalled();
    dispose();
  });
  it("offers no deletion of the last accessible project, or cancellation side effect", () => {
    const { api, edits, projects, dispose } = setup();
    projects([project("a"), { ...project("remote"), accessible: false }]);
    edits.openDelete("a", origin); expect(edits.prompt()).toBeUndefined();
    projects([project("a"), project("b")]); edits.openDelete("a", origin); edits.close();
    expect(api.deleteProject).not.toHaveBeenCalled();
    dispose();
  });
  it("rechecks names and identities before renaming and ignores reversion or unchanged values", async () => {
    const { api, edits, projects, dispose } = setup();
    await edits.rename("a", "a"); await edits.rename("a", " "); await edits.rename("missing", "rename");
    await edits.rename("a", "  Renamed  ");
    projects([{ ...project("a"), accessible: false }]); await edits.rename("a", "again");
    expect(api.renameProject).toHaveBeenCalledExactlyOnceWith("a", "Renamed");
    dispose();
  });
  it("resolves reorder neighbours at activation, including the end, and refuses boundary moves", async () => {
    const { api, edits, projects, dispose } = setup();
    await edits.move("a", -1); await edits.move("c", 1);
    await edits.move("b", -1); await edits.move("b", 1);
    expect(api.moveProject.mock.calls).toEqual([["b", "a"], ["b", undefined]]);
    projects([project("c"), project("b"), project("a")]);
    await edits.move("b", -1); expect(api.moveProject).toHaveBeenLastCalledWith("b", "c");
    await edits.drop("a", "c"); expect(api.moveProject).toHaveBeenLastCalledWith("a", "c");
    await edits.drop("a", "a"); await edits.drop("missing", "b"); await edits.drop("a", "missing");
    expect(api.moveProject).toHaveBeenCalledTimes(4);
    dispose();
  });
  it("allows only the mockup's one-line bounded project name", () => {
    expect(catalogName(" \t Example \t ")).toBe("Example");
    for (const value of ["", " \n ", "one\ntwo", "x".repeat(81)]) expect(catalogName(value)).toBeUndefined();
    expect(catalogName("x".repeat(80))).toHaveLength(80);
  });
});
