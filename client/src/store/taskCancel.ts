import { createSignal } from "solid-js";
import { cancelTaskExecution } from "../bridge";
import { notify } from "./actions";
import { tasks } from "./tasks";
import { setWritingTask, writingTask } from "./taskBoard";
import type { Task } from "../ui/screens/tasks-ui";
import { tr, trNoop } from "../i18n";

export interface Execution { dispatch_id: string; stage_at: number; machine: string; session_id: string }
export interface Cancellation { project: string; task: Task; execution: Execution; origin?: HTMLElement }
export const PAUSE_REASON = trNoop("Pausing an individual task is not available yet");
const CHANGED = trNoop("The task execution changed. Nothing was cancelled. Refresh the task.");
export const [taskCancellation, setTaskCancellation] = createSignal<Cancellation>();
export const canCancel = (task: Task) => task.writable && task.stage === "active" && !!task.dispatchId && task.stageAt > 0 && !!task.executor.machine && !!task.executor.id;
const executionOf = (t: Task): Execution => ({ dispatch_id: t.dispatchId, stage_at: t.stageAt, machine: t.executor.machine ?? "", session_id: t.executor.id });
const matches = (t: Task | undefined, execution: Execution): t is Task => !!t && canCancel(t) && t.dispatchId === execution.dispatch_id && t.stageAt === execution.stage_at && t.executor.machine === execution.machine && t.executor.id === execution.session_id;
const fresh = (project: string, uuid: string) => tasks.project() === project ? tasks.tasks().find((t) => t.uuid === uuid) : undefined;

/** Only opening this dialog is reversible; the confirm button owns the write. */
export function requestTaskCancellation(project: string, task: Task, origin?: HTMLElement) {
  if (writingTask() || taskCancellation()) return;
  const current = fresh(project, task.uuid), execution = executionOf(task);
  if (!matches(current, execution)) { notify("error", tr(CHANGED)); return; }
  setTaskCancellation({ project, task: { ...current, executor: { ...current.executor } }, execution, origin });
}

export async function confirmTaskCancellation(reason: string) {
  const pending = taskCancellation();
  if (!pending || writingTask()) return;
  setTaskCancellation(undefined);
  if (!matches(fresh(pending.project, pending.task.uuid), pending.execution)) { notify("error", tr(CHANGED)); return; }
  setWritingTask(pending.task.uuid);
  try {
    const error = await cancelTaskExecution(pending.project, pending.task.uuid, pending.execution, reason.trim());
    notify(error ? "error" : "info", error ? tr("Task cancelled. Cancel message not sent: %1", error) : tr("Task cancelled."));
  } catch (error) { notify("error", String(error)); }
  finally { setWritingTask(undefined); }
}
