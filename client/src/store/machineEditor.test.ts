import { describe, expect, it, vi } from "vitest";
import { createComputed, createRoot, createSignal } from "solid-js";
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });
vi.mock("../bridge", () => ({ machineChange: vi.fn(), machinePickKey: vi.fn(), machineSsh: vi.fn(), fleetRefresh: vi.fn(), machinesView: vi.fn(), machineResolve: vi.fn(), machineCheck: vi.fn() }));
import { createMachineEditor, LOCAL_MACHINE, NEW_MACHINE, type MachineTools } from "./machineEditor";
import { emptyMachines, type MachinesView } from "../types";

const view = (revision = "a".repeat(64)): MachinesView => ({ ...emptyMachines(), loaded: true, local: "fixture-local", connection_revision: revision,
  machines: [{ alias: "fixture-peer", hostname: "peer.invalid", user: "fixture", port: null, identity_file: "", enabled: true, source: "local", inherited: true, origin: "" }] });
const tick = () => new Promise<void>((done) => queueMicrotask(done));
function setup(write = vi.fn(async () => {}), tools?: MachineTools) {
  return createRoot((dispose) => {
    const [current, setView] = createSignal(view());
    const reload = vi.fn(async () => {}), changed = vi.fn(async () => {});
    const editor = createMachineEditor(current, write, reload, changed, tools);
    return { editor, current, setView, write, reload, changed, dispose };
  });
}

describe("Machines connection editor", () => {
  it("keeps exact unsaved fields and original revision through list updates without a reactive loop", () => {
    const t = setup();
    try {
      t.editor.choose("fixture-peer");
      let publications = 0;
      createRoot((dispose) => {
        createComputed(() => { t.editor.draft(); if (++publications > 12) throw new Error("draft feedback loop"); });
        t.editor.edit("hostname", "edited.invalid");
        t.setView(view("b".repeat(64)));
        expect(t.editor.draft()).toMatchObject({ hostname: "edited.invalid", revision: "a".repeat(64), dirty: true });
        t.editor.choose(NEW_MACHINE);
        t.editor.cancel();
        expect(t.editor.selected()).toBe("fixture-peer");
        expect(t.editor.draft().hostname).toBe("edited.invalid");
        expect(publications).toBeLessThan(5);
        dispose();
      });
    } finally { t.dispose(); }
  });
  it("Cancel preserves a draft and Discard applies the originally requested selection or refresh", () => {
    const t = setup();
    try {
      t.editor.choose(NEW_MACHINE);t.editor.edit("alias", "new-peer");
      t.editor.choose(LOCAL_MACHINE);
      expect(t.editor.prompt()).toEqual({ kind: "discard", next: LOCAL_MACHINE, refresh: false });
      t.editor.cancel();expect(t.editor.draft().alias).toBe("new-peer");
      t.editor.choose(NEW_MACHINE, true);t.editor.confirm();
      expect(t.reload).toHaveBeenCalledWith(true);
      expect(t.editor.draft().dirty).toBe(false);expect(t.editor.draft().alias).toBe("");
      expect(t.write).not.toHaveBeenCalled();
    } finally { t.dispose(); }
  });
  it("creates once with inherited blanks and the saved enable/background polling choice", async () => {
    const t = setup();
    try {
      t.editor.choose(NEW_MACHINE);t.editor.edit("alias", " new-peer ");t.editor.edit("enabled", false);
      expect(await t.editor.save()).toBe(true);
      expect(t.write).toHaveBeenCalledExactlyOnceWith({ alias: "new-peer", revision: "a".repeat(64), change: { kind: "set", create: true,
        connection: { hostname: "", user: "", port: null, identity_file: "", enabled: false } } });
      expect(t.editor.selected()).toBe("new-peer");expect(t.editor.draft().dirty).toBe(false);
      expect(t.changed).toHaveBeenCalledTimes(1);
    } finally { t.dispose(); }
  });
  it("refuses duplicate/local names and an invalid port before dispatch", async () => {
    const t = setup();
    try {
      t.editor.choose(NEW_MACHINE);
      for (const name of ["fixture-local", "fixture-peer", "-invalid", "a b"]) { t.editor.edit("alias", name);expect(await t.editor.save()).toBe(false); }
      t.editor.edit("alias", "new-peer");
      for (const port of ["65536", "22.5", "-1", "NaN"]) { t.editor.edit("port", port);expect(await t.editor.save()).toBe(false); }
      expect(t.write).not.toHaveBeenCalled();
    } finally { t.dispose(); }
  });
  it("binds removal to the confirmed original revision and keeps Cancel effect-free", async () => {
    const t = setup();
    try {
      t.editor.choose("fixture-peer");t.editor.remove();t.editor.cancel();
      expect(t.write).not.toHaveBeenCalled();
      t.editor.remove();t.setView(view("b".repeat(64)));t.editor.confirm();
      expect(t.write).not.toHaveBeenCalled();
      expect(t.editor.notice()?.text).toBe("The connection changed. Review it before removing.");
      t.editor.choose("fixture-peer", true);t.editor.remove();t.editor.confirm();await tick();await tick();
      expect(t.write).toHaveBeenCalledExactlyOnceWith({ alias: "fixture-peer", revision: "b".repeat(64), change: { kind: "remove" } });
      expect(t.editor.selected()).toBe(LOCAL_MACHINE);
    } finally { t.dispose(); }
  });
  it("a late close event from a discarded dialog cannot cancel a new confirmation", () => {
    const t = setup();
    try {
      t.editor.choose("fixture-peer");t.editor.remove();const original = t.editor.prompt()!;
      t.editor.cancel(original);t.editor.remove();const next = t.editor.prompt();
      expect(next).not.toBe(original);t.editor.cancel(original);
      expect(t.editor.prompt()).toBe(next);expect(t.write).not.toHaveBeenCalled();
    } finally { t.dispose(); }
  });
  it("holds one write, keeps unknown outcomes and drafts reviewable, and never retries automatically", async () => {
    let reject!: (error: Error) => void;
    const t = setup(vi.fn(() => new Promise<void>((_done, fail) => { reject = fail; })));
    try {
      t.editor.choose("fixture-peer");t.editor.edit("hostname", "edited.invalid");
      const first = t.editor.save();
      expect(t.editor.busy()).toBe(true);
      t.editor.choose(LOCAL_MACHINE);expect(t.editor.selected()).toBe("fixture-peer");
      expect(await t.editor.save()).toBe(false);
      reject(new Error("Could not confirm the connection change."));expect(await first).toBe(false);
      expect(t.write).toHaveBeenCalledTimes(1);expect(t.reload).not.toHaveBeenCalled();
      expect(t.editor.draft()).toMatchObject({ dirty: true, hostname: "edited.invalid", revision: "a".repeat(64) });
      expect(t.editor.notice()?.error).toBe(true);
    } finally { t.dispose(); }
  });
  it("keeps a cancelled key selection unchanged and applies an accepted path only to its original draft", async () => {
    const pickKey = vi.fn<() => Promise<string | null>>().mockResolvedValueOnce(null).mockResolvedValueOnce("/home/example/.ssh/id_fixture");
    const t = setup(undefined, { pickKey, ssh: vi.fn() });
    try {
      t.editor.choose(NEW_MACHINE);t.editor.edit("alias", "new-peer");
      const original = t.editor.draft();
      expect(await t.editor.pickKey()).toBe(false);expect(t.editor.draft()).toBe(original);
      expect(await t.editor.pickKey()).toBe(true);
      expect(t.editor.draft()).toMatchObject({ alias: "new-peer", identity_file: "/home/example/.ssh/id_fixture", dirty: true });
      expect(t.write).not.toHaveBeenCalled();
    } finally { t.dispose(); }
  });
  it("does not reseed the draft when an effective-connection or busy reply keeps the same source revision", async () => {
    const t = setup(undefined, { pickKey: vi.fn(async () => null), ssh: vi.fn() });
    try {
      t.editor.choose("fixture-peer");const original = t.editor.draft();
      t.setView({ ...t.current(), busy: true });
      t.setView({ ...t.current(), busy: false });
      expect(t.editor.draft()).toBe(original);
      expect(await t.editor.pickKey()).toBe(false);
      t.setView({ ...t.current() });
      expect(t.editor.draft()).toBe(original);
    } finally { t.dispose(); }
  });
  it("a pending key chooser holds selection and refuses a late result after the source revision changes", async () => {
    let choose!: (path: string) => void;
    const t = setup(undefined, { pickKey: vi.fn(() => new Promise<string>((done) => { choose = done; })), ssh: vi.fn() });
    try {
      t.editor.choose("fixture-peer");const original = t.editor.draft();
      const result = t.editor.pickKey();expect(t.editor.busy()).toBe(true);
      t.editor.choose(NEW_MACHINE);expect(t.editor.selected()).toBe("fixture-peer");
      t.setView(view("b".repeat(64)));choose("/home/example/.ssh/id_fixture");
      expect(await result).toBe(false);expect(t.editor.draft()).toBe(original);
      expect(t.editor.notice()?.error).toBe(true);expect(t.editor.busy()).toBe(false);
    } finally { t.dispose(); }
  });
  it("opens SSH once for the original saved disabled peer and never launches from a new or dirty draft", async () => {
    const ssh = vi.fn(async () => null);
    const t = setup(undefined, { pickKey: vi.fn(), ssh });
    try {
      t.setView({ ...view(), machines: view().machines.map((row) => ({ ...row, enabled: false })) });
      t.editor.choose("fixture-peer");expect(await t.editor.openSsh()).toBe(true);
      expect(ssh).toHaveBeenCalledExactlyOnceWith({ alias: "fixture-peer", revision: "a".repeat(64) });
      t.editor.edit("hostname", "edited.invalid");expect(await t.editor.openSsh()).toBe(false);
      t.editor.choose(NEW_MACHINE);t.editor.confirm();expect(await t.editor.openSsh()).toBe(false);
      expect(ssh).toHaveBeenCalledTimes(1);
    } finally { t.dispose(); }
  });

});
