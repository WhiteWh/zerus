import { tr } from "../../i18n";
import type { SelectionRow } from "./selection";
import type { Session } from "./types";

// zerus-sa9qn.17: the Terminate and Archive confirmations of a parent with live
// child sessions offer to end them too, off by default. Nothing here acts: it
// lists the descendants the confirmation names, as the list nests them
// (`parentIndex`), and which of them the same action applies to now. A link is
// never authority; the person's choice in the dialog is.

/** The deepest descendant level listed, as the list nests (spec 1.4) and `hgs --with-children` walks. */
export const CHILD_DEPTH = 8;

/**
 * The parent's live (non-archived) descendants on its own computer, breadth
 * first: every child before any grandchild. One pass over `rows` builds a
 * bounded child index; the walk never visits a session twice and stops after
 * CHILD_DEPTH levels. A session on another computer is never listed: its link
 * there is a claim this computer cannot verify.
 */
export function liveDescendants(parent: Pick<Session, "key" | "computer">, rows: Iterable<Session>, parentKey: (s: Session) => string | undefined): Session[] {
  const children = new Map<string, Session[]>();
  for (const s of rows) {
    if (s.computer !== parent.computer || s.state === "archived") continue;
    const up = parentKey(s);
    if (!up || up === s.key) continue;
    const list = children.get(up);
    if (list) list.push(s); else children.set(up, [s]);
  }
  const seen = new Set([parent.key]);
  const out: Session[] = [];
  let level = [parent.key];
  for (let depth = 0; depth < CHILD_DEPTH && level.length; depth++) {
    const next: string[] = [];
    for (const key of level) {
      for (const s of children.get(key) ?? []) {
        if (seen.has(s.key)) continue;
        seen.add(s.key); out.push(s); next.push(s.key);
      }
    }
    level = next;
  }
  return out;
}

export type ChildAction = "terminate" | "archive";

/** The descendants a confirmation names: those the same action applies to now, and those it leaves as they are. */
export interface ChildOffer { ending: SelectionRow[]; staying: SelectionRow[] }

export function childOffer(descendants: readonly SelectionRow[], applies: (s: SelectionRow) => boolean): ChildOffer {
  const ending: SelectionRow[] = [], staying: SelectionRow[] = [];
  for (const s of descendants) (applies(s) ? ending : staying).push(s);
  return { ending, staying };
}

/** The opt-in's label: what the action does to the children, by count. */
export function childChoiceText(a: ChildAction, n: number): string {
  if (a === "terminate") return n === 1 ? tr("Also end 1 child session") : tr("Also end %n child sessions", n);
  return n === 1 ? tr("Also archive 1 child session") : tr("Also archive %n child sessions", n);
}

/** How the chosen children run, under the opt-in. */
export function childHintText(a: ChildAction): string {
  return a === "terminate"
    ? tr("Each one stops through its own Terminate after this session ends. Their working folders are kept.")
    : tr("Each one moves to Archive through its own Move to archive after this session does.");
}

/**
 * The children the action does not apply to now (Terminate takes running
 * sessions, Archive stopped or paused ones; an offline computer, a write in
 * flight or an agent without the action leaves a session out too).
 */
export function childStayText(a: ChildAction, n: number): string {
  if (a === "terminate") return n === 1 ? tr("1 child session cannot be terminated now and stays as it is:") : tr("%n child sessions cannot be terminated now and stay as they are:", n);
  return n === 1 ? tr("1 child session cannot be archived now and stays as it is:") : tr("%n child sessions cannot be archived now and stay as they are:", n);
}
