import { createSignal } from "solid-js";
import { taskBoardAction } from "../bridge";
import { notify } from "./actions";
import { tasks } from "./tasks";
import { STAGES, type Task } from "../ui/screens/tasks-ui";
import { tr, trNoop } from "../i18n";

export type BoardAction = { kind: "to_backlog" | "to_todo" | "back_to_todo" | "accept" | "top" } | { kind: "reorder"; anchor: string; before: boolean };
export type MenuAction = Exclude<BoardAction["kind"], "reorder">;
export const ACTION_LABEL: Record<MenuAction, string> = { to_backlog: trNoop("Move to Backlog"), to_todo: trNoop("Move to To do"), back_to_todo: trNoop("Back to To do"), accept: trNoop("Accept"), top: trNoop("Move to the top of To do") };
export const actionLabel = (action: MenuAction): string => tr(ACTION_LABEL[action]);
export const ACTION_ICON: Record<MenuAction, string> = { to_backlog: "clock", to_todo: "list-todo", back_to_todo: "undo-2", accept: "check", top: "arrow-up-to-line" };
export const [writingTask, setWritingTask] = createSignal<string>();
export function boardActions(stage: string): MenuAction[] {
  return stage === "todo" ? ["to_backlog", "top"] : stage === "backlog" ? ["to_todo"] : stage === "review" ? ["back_to_todo", "accept"] : stage === "done" ? ["back_to_todo"] : [];
}
export const primaryAction = (stage: string): MenuAction | undefined => stage === "review" ? "accept" : stage === "done" ? "back_to_todo" : undefined;
export const canDrag = (task: Task) => task.writable && (STAGES as readonly string[]).includes(task.stage);
export const stageDropAction = (task: Task | undefined, stage: string): BoardAction | undefined => task?.writable && task.stage === "todo" && stage === "backlog" ? { kind: "to_backlog" }
  : task?.writable && task.stage === "backlog" && stage === "todo" ? { kind: "to_todo" } : undefined;

export async function actOnTask(project: string, task: Task, action: BoardAction) {
  if (writingTask()) return;
  const fresh = tasks.project() === project ? tasks.tasks().find((t) => t.uuid === task.uuid) : undefined;
  const anchor = action.kind === "reorder" ? tasks.tasks().find((t) => t.uuid === action.anchor) : undefined;
  if (!fresh || !canDrag(fresh) || fresh.stage !== task.stage || (action.kind === "reorder"
    ? !anchor || !anchor.writable || anchor.uuid === task.uuid || anchor.stage !== fresh.stage
    : !boardActions(fresh.stage).includes(action.kind))) {
    notify("error", tr("The task changed. Nothing was changed. Refresh the task.")); return;
  }
  // No optimistic mutation or retry: the existing stream publishes the result.
  const expectedStage = fresh.stage;
  setWritingTask(task.uuid);
  try { await taskBoardAction(project, task.uuid, expectedStage, action); }
  catch (error) { notify("error", String(error)); }
  finally { setWritingTask(undefined); }
}

/** Index the visible bounded rows; dropping beside the source is a no-op. */
export function reorderGap(rows: readonly { uuid: string; top: number; height: number }[], source: string, y: number) {
  let gap = rows.findIndex((r) => y < r.top + r.height / 2);
  if (gap < 0) gap = rows.length;
  const from = rows.findIndex((r) => r.uuid === source);
  if (!rows.length || from >= 0 && (gap === from || gap === from + 1)) return undefined;
  const before = gap < rows.length, anchor = rows[before ? gap : rows.length - 1].uuid;
  return { gap, anchor, before };
}
