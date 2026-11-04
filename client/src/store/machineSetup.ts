import { createRoot, createSignal, type Accessor } from "solid-js";
import { machinePickSource, machineSetup, fleetRefresh } from "../bridge";
import { machineEditor, LOCAL_MACHINE, type createMachineEditor } from "./machineEditor";
import { machineCards, machinesPage, loadMachines, testConnection } from "./machines";
import type { MachineMeta, MachinesView, MachineReference, MachineSetupOutcome, MachineSetupRequest } from "../types";
import { tr } from "../i18n";

export type SetupPrompt = { kind: "install"; reference: MachineReference } | { kind: "enable"; machine_id: string };
type Editor = ReturnType<typeof createMachineEditor>;
interface Tools { setup: (request: MachineSetupRequest) => Promise<MachineSetupOutcome>; pick: () => Promise<string | null>; refresh: () => Promise<unknown>; check: (alias: string) => Promise<MachinesView | undefined> }
export function createMachineSetup(editor: Editor, view: Accessor<MachinesView>, local: Accessor<MachineMeta | undefined>, tools: Tools) {
  const [prompt, setPrompt] = createSignal<SetupPrompt>();
  const [busy, setBusy] = createSignal(false);
  const [choosing, setChoosing] = createSignal(false);
  const [source, setSource] = createSignal("");
  const [result, setResult] = createSignal<{ target: string; error: boolean; text: string; output: string; followup?: boolean }>();
  const canEnable = () => local()?.platform === "windows" && local()?.local_sessions === false && !!local()?.local_host && !!local()?.machine_id;
  const open = (kind: "install" | "enable") => {
    if (busy() || editor.busy() || prompt()) return;
    if (kind === "install") {
      if (editor.selected() === LOCAL_MACHINE || editor.isNew() || editor.draft().dirty || !editor.draft().revision) return;
      setPrompt({ kind, reference: { alias: editor.selected(), revision: editor.draft().revision } });
    } else {
      if (editor.selected() !== LOCAL_MACHINE || !canEnable()) return;
      setPrompt({ kind, machine_id: local()!.machine_id! });
    }
    editor.clearNotice();
    setSource("");
  };
  const close = (original: SetupPrompt) => { if (prompt() === original) setPrompt(undefined); };
  const pick = async (original: SetupPrompt) => {
    if (choosing() || prompt() !== original) return;
    setChoosing(true);
    try { const path = await tools.pick();if (path !== null && prompt() === original) setSource(path); }
    catch (error) { if (prompt() === original) setResult({ target: editor.selected(), error: true, text: String(error), output: "" }); }
    finally { setChoosing(false); }
  };
  const confirm = async (original: SetupPrompt) => {
    if (prompt() !== original || busy() || choosing() || editor.busy()) return false;
    const target = original.kind === "install" ? original.reference.alias : LOCAL_MACHINE;
    if (editor.selected() !== target || (original.kind === "install"
      ? editor.draft().dirty || view().connection_revision !== original.reference.revision
      : !canEnable() || local()?.machine_id !== original.machine_id)) {
      setResult({ target: editor.selected(), error: true, text: tr("Machine settings changed elsewhere. Reload the page before setup."), output: "" });close(original);return false;
    }
    const request: MachineSetupRequest = original.kind === "install" ? { ...original, source: source().trim() } : original;
    setBusy(true);close(original);
    setResult({ target, error: false, text: original.kind === "install" ? tr("Installing on %1… Keep Zerus running until setup finishes.", target) : tr("Enabling local sessions on this computer…"), output: "" });
    try {
      const outcome = await tools.setup(request);
      setResult({ target, error: !outcome.success, text: outcome.text, output: outcome.output });
      if (outcome.success) {
        if (original.kind === "install") {
          await tools.check(target);
          setResult({ target, error: false, text: outcome.text, output: outcome.output, followup: true });
        }
        await tools.refresh();
      }
      return outcome.success;
    } catch (error) { setResult({ target, error: true, text: String(error), output: "" });return false; }
    finally { setBusy(false); }
  };
  return { prompt, busy, choosing, source, setSource, result, canEnable, open, close, pick, confirm };
}
export const machineSetupEditor = createRoot(() => createMachineSetup(machineEditor, machinesPage, () => machineCards[""], {
  setup: machineSetup, pick: machinePickSource,
  refresh: async () => { await loadMachines(true);await fleetRefresh(); }, check: async (alias) => { await testConnection(alias);return machinesPage(); },
}));
