import { createSignal } from "solid-js";
import type { Action, Target } from "../types";
import type { ActionRow } from "../ui/sessions/actions-ui";

export type EndAction = Action | { kind: "clear_context"; dismiss_prompt: boolean };
export interface EndCapture { target: Target; action: EndAction; state: string }
export interface EndReference { id: string; index: number }
const [pending, setPending] = createSignal<ReadonlySet<string>>(new Set());
export const endKey = (computer: string, name: string) => `${computer}\n${name}`;
export const pendingEnd = (computer: string, name: string) => pending().has(endKey(computer, name));
export function claimEnds(captures: EndCapture[]): (() => void) | undefined {
  const keys = new Set(captures.map((c) => endKey(c.target.computer, c.target.name)));
  if ([...keys].some((k) => pending().has(k))) return;
  setPending(new Set([...pending(), ...keys]));
  return () => setPending((all) => new Set([...all].filter((k) => !keys.has(k))));
}
export const isEndAction = (action: Action) => ["archive", "terminate", "kill", "terminate_shell", "forget", "forget_binding", "forget_archive", "clear_context"].includes(action.kind);
export const captureEnd = (row: ActionRow, action: EndAction): EndCapture => ({
  target: { computer: row.computer, name: row.name, run_id: row.runId ?? "", conversation_id: row.conversationId ?? "" }, action, state: row.state,
});
type EndIdentity = Pick<ActionRow, "computer" | "name" | "runId" | "conversationId" | "state" | "archiveId">;
export const sameEnd = (a: EndIdentity, b: EndIdentity | undefined) => !!b && a.computer === b.computer && a.name === b.name && a.runId === b.runId
  && a.conversationId === b.conversationId && a.state === b.state && a.archiveId === b.archiveId;
