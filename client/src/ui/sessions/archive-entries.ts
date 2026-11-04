import { inScope } from "../../store/archive";
import type { MenuEntry } from "../../store/ui";
import type { Session } from "./types";
import { trNoop } from "../../i18n";

// Clear archive in the batch menu, after Qt's updateSelectionActions
// (SessionSelection.cpp 171-181) and clearArchive (226-235). The client's
// archive is always paged, so the entry names the loaded archived sessions;
// it runs the selection's Forget (store/batch.ts) with Qt's Clear archive
// confirmation. It only reads what it is given; selection-live.ts binds it
// to the stores.

/** Qt's paged label (`m_archiveBrowser->paged()`). */
export const CLEAR_ARCHIVE_LABEL = trNoop("Remove loaded archived sessions…");

type Archived = Pick<Session, "state" | "machine"> & { archivedAt: number; group: string };

/** The machine and project filters (Qt's host and project filters; the client has no account filter). */
export interface ClearScope {
  machines: readonly string[];
  /** The organization's project IDs. */
  projects: readonly string[];
}

/** Qt clears only the archive identities inside the selected projects and machines. */
const inFilters = (s: Archived, scope: ClearScope) => s.state === "archived" && (!scope.projects.length || scope.projects.includes(s.group)) && inScope(s, scope);

/**
 * Qt's clearArchive entries: the loaded archived rows in the machine and
 * project filters and inside the date range. Whether each can be forgotten
 * is the batch's question; the others are "left as they are".
 */
export function clearArchiveRows<T extends Archived>(rows: readonly T[], scope: ClearScope & { includes(archivedAt: number): boolean }): T[] {
  return rows.filter((s) => inFilters(s, scope) && scope.includes(s.archivedAt));
}

/**
 * The entry: enabled when no batch runs and some loaded archived row in the
 * machine and project filters can be forgotten (Qt counts without the date
 * range). `forgettable` is the batch's Forget applicability.
 */
export function clearArchiveEntry<T extends Archived>(rows: readonly T[], scope: ClearScope, forgettable: (s: T) => boolean, batchRunning: boolean, run: () => void): Extract<MenuEntry, { label: string }> {
  const some = rows.some((s) => inFilters(s, scope) && forgettable(s));
  return { label: CLEAR_ARCHIVE_LABEL, icon: "trash", danger: true, disabled: !some || batchRunning, action: run };
}
