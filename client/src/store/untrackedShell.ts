import * as bridge from "../bridge";
import type { Action } from "../types";
import type { ActionRow } from "../ui/sessions/actions-ui";
import { trNoop } from "../i18n";

export const UNVERIFIED_SHELL = trNoop("This session is not tracked by Zerus and its terminal could not be verified. Open its terminal and close it with exit after its jobs finish.");
export const untrackedShell = (row: ActionRow) => row.state === "running" && !row.tracked && !row.runId && !row.conversationId
  && ["", "shell", "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh"].includes(row.cmd);

/** Called when the confirmation opens; the original proof stays with that dialog. */
export async function captureUntrackedShell(row: ActionRow): Promise<Extract<Action, { kind: "terminate_shell" }>> {
  if (!untrackedShell(row)) throw UNVERIFIED_SHELL;
  const identity = await bridge.untrackedShellIdentity({ computer: row.computer, name: row.name, run_id: "", conversation_id: "" });
  return { kind: "terminate_shell", identity };
}
