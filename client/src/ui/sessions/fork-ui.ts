import type { Details } from "../../types";
import { renameParts, tagProblem, type ActionRow } from "./actions-ui";
import { trNoop, tr } from "../../i18n";

export const FORK_HINT = trNoop("Create an independent conversation with the same context and working folder");
export const FORK_CHECKING = trNoop("Checking whether this session can be forked…");
export const FORK_CHANGED = trNoop("The session changed. Open its actions again.");

/** Qt's fork_available, with the same identity gate as the selected inspection. */
export function forkAvailable(row: ActionRow, details: Details | undefined, online: boolean, busy: boolean): boolean {
  return online && !busy && !!details?.fork_supported && !!details.conversation_id
    && (!row.runId || row.runId === details.run_id) && (!row.conversationId || row.conversationId === details.conversation_id);
}

export function sameForkSource(a: ActionRow, b: ActionRow): boolean {
  return a.key === b.key && a.computer === b.computer && a.name === b.name && a.state === b.state
    && a.runId === b.runId && a.conversationId === b.conversationId && a.created === b.created && a.archiveId === b.archiveId;
}

export function forkName(row: ActionRow, tag: string): string { return renameParts(row.name).prefix + tag; }
export function forkProblem(row: ActionRow, tag: string, rows: readonly ActionRow[]): string | undefined {
  return tagProblem(tag) ?? (rows.some((r) => r.computer === row.computer && r.name === forkName(row, tag)) ? tr("This session name is already in use.") : undefined);
}
export function forkTag(row: ActionRow, rows: readonly ActionRow[]): string {
  const tag = renameParts(row.name).tag, base = tag ? `${tag}-fork` : "fork";
  const names = new Set(rows.filter((r) => r.computer === row.computer).map((r) => r.name));
  let value = base;
  for (let number = 2; names.has(forkName(row, value)); number++) value = `${base}-${number}`;
  return value;
}
