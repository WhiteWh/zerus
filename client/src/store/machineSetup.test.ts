import { describe, it, expect, vi } from "vitest";
import { createRoot, createSignal } from "solid-js";
vi.hoisted(() => vi.stubGlobal("location", new URL("http://localhost/")));
vi.mock("../bridge", () => ({ machineChange: vi.fn(), machinePickKey: vi.fn(), machineSsh: vi.fn(), machineSetup: vi.fn(), machinePickSource: vi.fn(), fleetRefresh: vi.fn(), machinesView: vi.fn(), machineResolve: vi.fn(), machineCheck: vi.fn() }));
import { createMachineEditor, LOCAL_MACHINE } from "./machineEditor";
import { createMachineSetup } from "./machineSetup";
import { emptyMachines, type MachineMeta, type MachineSetupOutcome } from "../types";
const success: MachineSetupOutcome = { success: true, uncertain: false, text: "HGS installed. Checking connection…", output: "fixture setup complete" };
function fixture() {
  return createRoot((dispose) => {
    const [view, setView] = createSignal({ ...emptyMachines(), loaded: true, local: "local", connection_revision: "a".repeat(64), machines: [{ alias: "peer.invalid", hostname: "peer.invalid", user: "", port: null, identity_file: "", enabled: true, source: "local", inherited: false, origin: "" }] });
    const [local, setLocal] = createSignal<MachineMeta>({ machine_id: "original-local", platform: "windows", local_sessions: false, local_host: {}, host: "local" } as MachineMeta);
    const tools = { setup: vi.fn(async () => success), pick: vi.fn(async (): Promise<string | null> => null), refresh: vi.fn(async () => {}), check: vi.fn(async () => undefined) };
    const editor = createMachineEditor(view, vi.fn(), vi.fn(), vi.fn());
    return { editor, setup: createMachineSetup(editor, view, local, tools), tools, setView, view, setLocal, local, dispose };
  });
}
describe("confirmed machine setup", () => {
  it("Cancel makes no call; confirmation uses only the original alias, revision and source", async () => {
    const f = fixture();try {
      f.editor.choose("peer.invalid");f.setup.open("install");f.setup.close(f.setup.prompt()!);expect(f.tools.setup).not.toHaveBeenCalled();
      f.setup.open("install");f.setup.setSource("/source with spaces");expect(await f.setup.confirm(f.setup.prompt()!)).toBe(true);
      expect(f.tools.setup).toHaveBeenCalledExactlyOnceWith({ kind: "install", reference: { alias: "peer.invalid", revision: "a".repeat(64) }, source: "/source with spaces" });expect(f.tools.check).toHaveBeenCalledExactlyOnceWith("peer.invalid");
    } finally { f.dispose(); }
  });
  it("a changed revision refuses before setup; a cancelled chooser cannot populate a replacement dialog", async () => {
    const f = fixture();try {
      f.editor.choose("peer.invalid");f.setup.open("install");const original = f.setup.prompt()!;
      let done!: (path: string) => void;f.tools.pick.mockImplementationOnce(() => new Promise((resolve) => { done = resolve; }));
      const picking = f.setup.pick(original);f.setup.close(original);f.setup.open("install");done("/late/source");await picking;expect(f.setup.source()).toBe("");
      const current = f.setup.prompt()!;f.setView({ ...f.view(), connection_revision: "b".repeat(64) });expect(await f.setup.confirm(current)).toBe(false);expect(f.tools.setup).not.toHaveBeenCalled();
    } finally { f.dispose(); }
  });
  it("an unknown outcome stays visible and never triggers retry or connection checks", async () => {
    const f = fixture();try {
      f.tools.setup.mockResolvedValueOnce({ success: false, uncertain: true, text: "Setup result is unknown.", output: "original output" });
      f.editor.choose("peer.invalid");f.setup.open("install");const original = f.setup.prompt()!;expect(await f.setup.confirm(original)).toBe(false);
      expect(await f.setup.confirm(original)).toBe(false);expect(f.tools.setup).toHaveBeenCalledTimes(1);expect(f.tools.check).not.toHaveBeenCalled();expect(f.setup.result()?.output).toBe("original output");
    } finally { f.dispose(); }
  });
  it("local enable requires the original Windows console identity and one explicit confirmation", async () => {
    const f = fixture();try {
      expect(f.editor.selected()).toBe(LOCAL_MACHINE);f.setup.open("enable");const original = f.setup.prompt()!;f.setLocal({ ...f.local(), machine_id: "replacement" });expect(await f.setup.confirm(original)).toBe(false);expect(f.tools.setup).not.toHaveBeenCalled();
      f.setup.open("enable");expect(await f.setup.confirm(f.setup.prompt()!)).toBe(true);expect(f.tools.setup).toHaveBeenCalledExactlyOnceWith({ kind: "enable", machine_id: "replacement" });
    } finally { f.dispose(); }
  });
});
