import type { ComposerView, HeldInputView, HeldMessageView, TerminalDraftView } from "../../types";
import type { Session } from "./types";

/**
 * The message the selected session's composer holds while the desktop
 * service compacts before sending it (zerus-pabah). It follows the open
 * composer's own view, as the composer and its queue do, with no draft-key
 * comparison: a key or route difference must never hide it (zerus-i0ak2).
 * Only a main composer of a live session shows it, never a child's.
 */
export function heldMessage(held: HeldMessageView | null | undefined, view: Pick<ComposerView, "subagent"> | null | undefined, s: Session | undefined, child: boolean): HeldMessageView | undefined {
  if (!held || !view || !s || child || view.subagent || s.state === "archived") return undefined;
  return held;
}

/** The bars of a composer that shows none: one value, so a memo over it stays unchanged. */
const NONE: readonly HeldInputView[] = Object.freeze([]);

/** Only a main composer of a live session shows the held messages and the Terminal draft, never a child's. */
const mainLive = (view: Pick<ComposerView, "subagent"> | null | undefined, s: Session | undefined, child: boolean) =>
  !!view && !!s && !child && !view.subagent && s.state !== "archived";

/**
 * the messages hgs holds for the selected session until its agent takes
 * the previous one (Qt's HeldInputBar), oldest first, from the open
 * composer's own view as the held message above.
 */
export function heldInputs(rows: readonly HeldInputView[] | null | undefined, view: Pick<ComposerView, "subagent"> | null | undefined, s: Session | undefined, child: boolean): readonly HeldInputView[] {
  return rows?.length && mainLive(view, s, child) ? rows : NONE;
}

/** Text left in the agent's own Terminal input (Qt's TerminalDraftBar), under the same rule. */
export function terminalDraftBar(bar: TerminalDraftView | null | undefined, view: Pick<ComposerView, "subagent"> | null | undefined, s: Session | undefined, child: boolean): TerminalDraftView | undefined {
  return bar && mainLive(view, s, child) ? bar : undefined;
}

/** Qt's QString::simplified, for a one-line preview: whitespace runs become one space, trimmed. */
export function simplified(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

/** The local rows with the held message's own row left out: the held entry stands for it until it is sent. */
export function withoutHeld<T extends { id: string }>(rows: readonly T[], held: HeldMessageView | null | undefined): readonly T[] {
  return held ? rows.filter((row) => row.id !== held.request_id) : rows;
}
