import type { TaskEntry } from "../../store/taskDetails";
import { STAGE_LABEL, type Stage } from "./tasks-ui";
import { tr } from "../../i18n";

/** TaskDialogs::taskHistoryAction and TaskInspector's discussion receipts. */
export function historyAction(e: TaskEntry): string {
  const words: Record<string, string> = { created: "created the task", edited: "edited the prompt", cancelled: "cancelled it",
    resumed: "requested changes and submitted feedback", started: "started the task" };
  return words[e.kind] ?? (e.kind === "title" ? `renamed the task: ${e.text}` : e.kind === "stage" ? `moved it to ${STAGE_LABEL[e.text as Stage] ?? e.text}`
    : e.kind === "target" ? `changed the session: ${e.text}` : e.kind === "sent" ? `sent it to ${e.session}`
    : e.kind === "message_delivery" ? `message ${e.requestId} — ${e.delivery.state}` : e.text || e.kind);
}
export function deliveryText(e: TaskEntry, worker: string): string {
  const d = e.delivery;
  return e.kind === "comment" || d.state === "note" ? tr("Saved note — not sent") : d.state === "queued" ? tr("Waiting for Start")
    : d.state === "submitted" ? d.mode === "queue" ? tr("Included in Start request") : worker ? tr("Submitted to coordinator — child receipt unavailable") : tr("Submitted to executor")
    : d.state === "failed" ? tr("Saved in task — not sent") : tr("Saved in task — delivery unconfirmed");
}
export function deliveryError(e: TaskEntry): string {
  return /execution changed|identity changed|conversation changed|dispatch receipt|dispatched run/.test(e.delivery.error)
    ? tr("Your comment is saved here. The task’s previous executor could not be verified, so it was not sent. Open executor Activity to continue the discussion, or return the task to To do and start it with the current executor.")
    : e.delivery.error;
}
