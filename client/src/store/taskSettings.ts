import { createSignal } from "solid-js";
import { readTaskSettings, saveTaskSettings } from "../bridge";
import { notify } from "./actions";
import { trNoop } from "../i18n";

export interface TaskSettings { task_template: string; cancel_template: string; agents_other: boolean; agent_limit: number }
export interface TaskSettingsDialog { project: string; name: string; base: TaskSettings; origin?: HTMLElement }
export const SETTINGS_LIMIT_ERROR = trNoop("Limit tasks started by agents: use 0 to 1000 per hour.");
const keys = ["task_template", "cancel_template", "agents_other", "agent_limit"] as const;
export function settingsChanges(base: TaskSettings, next: TaskSettings): Partial<TaskSettings> {
  return Object.fromEntries(keys.filter((key) => base[key] !== next[key]).map((key) => [key, next[key]]));
}
export function createTaskSettings(port: { read(project: string): Promise<TaskSettings>; save(project: string, changes: Partial<TaskSettings>): Promise<TaskSettings> }, report: (text: string) => void) {
  const [dialog, setDialog] = createSignal<TaskSettingsDialog>(), [busy, setBusy] = createSignal(false), [error, setError] = createSignal("");
  const open = async (project: string, name: string, origin?: HTMLElement) => {
    if (busy() || dialog()) return;
    setBusy(true); setError("");
    try { const base = await port.read(project); setDialog({ project, name, base, origin }); }
    catch (e) { report(String(e)); }
    finally { setBusy(false); }
  };
  const close = (current: TaskSettingsDialog) => { if (dialog() === current) setDialog(undefined); };
  const save = async (current: TaskSettingsDialog, next: TaskSettings): Promise<boolean> => {
    if (busy() || dialog() !== current) return false;
    if (!Number.isInteger(next.agent_limit) || next.agent_limit < 0 || next.agent_limit > 1000) { setError(SETTINGS_LIMIT_ERROR); return false; }
    const changes = settingsChanges(current.base, next);
    if (!Object.keys(changes).length) { close(current); return true; }
    setBusy(true); setError("");
    try { await port.save(current.project, changes); close(current); return true; }
    catch (e) { if (dialog() === current) setError(String(e)); else report(String(e)); return false; }
    finally { setBusy(false); }
  };
  return { dialog, busy, error, open, close, save };
}
export const taskSettings = createTaskSettings({ read: (project) => readTaskSettings(project), save: (project, changes) => saveTaskSettings(project, changes) }, (text) => notify("error", text, true));
