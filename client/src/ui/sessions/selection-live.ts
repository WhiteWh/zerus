import { machineFilter, multi, projectFilter, type MenuEntry } from "../../store/ui";
import { tr } from "../../i18n";
import { catalog, computerOffline } from "../../store/catalog";
import { updating } from "../../store/actions";
import { archive } from "../../store/archive";
import { applies, run, running, runMachine } from "../../store/batch";
import { clearArchiveEntry, clearArchiveRows } from "./archive-entries";
import { clearMulti } from "./organize";
import type { Row } from "./model";
import type { Session } from "./types";
import { MACHINES_WAITING, machineEntries } from "./machine-batch";
import { countable, selectionMenu, visibleSelected } from "./selection-ui";
import { selectionSnapshot, type SelectionAction, type SelectionRow } from "./selection";
import { liveNameIndex } from "./actions-ui";

// The live (store-bound) halves of the selection bar and the batch menu; the
// pure models are in selection-ui.ts, machine-batch.ts and archive-entries.ts.

export const TOOL_WIDTH = 34;

export const selectedRows = (listed: readonly Row[]): Session[] => visibleSelected(listed, multi);
/** Per-action counts over the selection (Qt's selectionActionApplies); archived names are checked against one live name index. */
export const selectionCount = (rows: SelectionRow[]) => {
  let names: Set<string> | undefined;
  const index = () => (names ??= liveNameIndex(catalog.rows));
  return (a: SelectionAction) =>
    rows.filter((s) => countable(s, a, a === "move" || applies(s, a, !computerOffline(s.computer), updating(s.computer, s.name), index))).length;
};
export const runSelection = (rows: SelectionRow[], a: SelectionAction) => { void run(a, rows.map(selectionSnapshot)); };
/** The context menu of a row inside a multi-selection (Qt populateSelectionMenu). */
export const selectionContextMenu = (listed: readonly Row[]): MenuEntry[] => {
  const rows = selectedRows(listed).map(selectionSnapshot);
  return selectionMenu(selectionCount(rows), rows.length, running(), (a) => runSelection(rows, a), clearMulti);
};

/** Pause all and Resume all per computer (Qt's populateBatchActions): this computer first, then the peers. */
export function machineMenu(): MenuEntry[] {
  const computers = new Set<string>(Object.keys(catalog.computers));
  const rows: Record<string, Session[]> = {};
  for (const id of catalog.order) {
    const r = catalog.rows[id];
    if (!r) continue;
    computers.add(r.computer);
    (rows[r.computer] ??= []).push(r);
  }
  const sorted = [...computers].sort((a, b) => (a === "" ? -1 : b === "" ? 1 : 0));
  const entries = sorted.flatMap((computer): MenuEntry[] => {
    const own = rows[computer] ?? [];
    const machine = own[0]?.machine || computer || "this computer";
    return machineEntries({
      machine, computer, online: !computerOffline(computer), batchRunning: running(), rows: own,
      start: (kind, count) => { void runMachine(computer, machine, kind, count); },
    });
  });
  return entries.length ? [...entries, { separator: true }] : [{ label: tr(MACHINES_WAITING), disabled: true }, { separator: true }];
}

/**
 * Clear archive (Qt's clearArchive): the loaded archived rows in the machine
 * and project filters, read again at the click with the date range, go to the
 * selection's Forget with the Clear archive confirmation.
 */
export function clearArchiveMenuEntry(): MenuEntry {
  const scope = () => ({ machines: machineFilter(), projects: projectFilter() });
  const forgettable = (s: Session) => applies(s, "forget", !computerOffline(s.computer), updating(s.computer, s.name));
  return clearArchiveEntry(archive.rows(), scope(), forgettable, running(), () => {
    const rows = clearArchiveRows(archive.rows(), { ...scope(), includes: archive.includes });
    // The client's archive is always paged: Qt's paged wording.
    void run("forget", rows.map(selectionSnapshot), { clearArchive: true, paged: true });
  });
}
