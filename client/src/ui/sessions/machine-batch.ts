import type { MenuEntry } from "../../store/ui";
import { trNoop, tr } from "../../i18n";

// The per-machine Pause all and Resume all entries of the batch menu, after
// Qt's populateBatchActions (SessionsWindow.cpp 2844-2893). The client's menu
// has no submenus, so each machine is a heading followed by its entries.

export const NATIVE_LINE = "OpenCode uses individual Pause / Resume controls";
export const NATIVE_HINT = "OpenCode sessions are excluded; use their individual Pause / Resume controls.";
export const MIXED_STATE = trNoop("Some sessions are busy or context is not tracked");
export const MACHINES_WAITING = trNoop("Waiting for machines…");

/** What the counts read from a catalog row. */
export interface MachineRow {
  state: string;
  cmd: string;
  bulkLifecycle: boolean;
  resumable: boolean;
  activity: string;
  processState: string;
}

export interface MachineCounts { live: number; saved: number; allIdle: boolean; nativeControls: boolean }

/** Live and saved rows of one computer; archived rows are skipped, OpenCode and rows without bulk lifecycle only set `nativeControls`. */
export function machineCounts(rows: readonly MachineRow[]): MachineCounts {
  let live = 0;
  let saved = 0;
  let allIdle = true;
  let nativeControls = false;
  for (const s of rows) {
    if (s.state === "archived") continue;
    if (s.cmd === "opencode" || !s.bulkLifecycle) { nativeControls = true; continue; }
    if (s.state === "running") {
      live++;
      allIdle = allIdle && s.resumable && s.activity === "idle" && s.processState !== "exited";
    } else if (s.resumable) saved++;
  }
  return { live, saved, allIdle, nativeControls };
}

export interface MachineInput {
  /** The name the machine's rows show. */
  machine: string;
  computer: string;
  online: boolean;
  /** A batch or machine-wide call is running (Qt's m_pending). */
  batchRunning: boolean;
  rows: readonly MachineRow[];
  /** Starts the call; `count` is the live or saved sessions counted. */
  start(kind: "pause_all" | "resume_all", count: number): void;
}

type Entry = Extract<MenuEntry, { label: string }>;

/** One machine's heading and entries, in Qt's order; all disabled while it is offline. */
export function machineEntries(m: MachineInput): MenuEntry[] {
  const { live, saved, allIdle, nativeControls } = machineCounts(m.rows);
  const out: MenuEntry[] = [{ heading: m.machine }];
  if (nativeControls) out.push({ label: NATIVE_LINE, disabled: true });
  const pause: Entry = {
    label: tr("Pause all (%1)", live),
    disabled: !(m.online && !m.batchRunning && live > 0 && allIdle),
    action: () => m.start("pause_all", live),
  };
  out.push(pause);
  if (live > 0 && !allIdle) out.push({ label: tr(MIXED_STATE), disabled: true });
  const resume: Entry = {
    label: tr("Resume all in background (%1)", saved),
    disabled: !(m.online && !m.batchRunning && saved > 0),
    action: () => m.start("resume_all", saved),
  };
  if (nativeControls) { pause.hint = NATIVE_HINT; resume.hint = NATIVE_HINT; }
  out.push(resume);
  if (!m.online) for (const e of out) if ("label" in e) e.disabled = true;
  return out;
}
