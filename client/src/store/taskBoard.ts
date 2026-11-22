import { createSignal } from "solid-js";
import { taskBoardAction } from "../bridge";
import { notify } from "./actions";
import { tasks } from "./tasks";
import { STAGES, type Task } from "../ui/screens/tasks-ui";
import { tr, trNoop } from "../i18n";

export type BoardAction = { kind: "to_backlog" | "to_todo" | "back_to_todo" | "accept" | "top" }
  | { kind: "reorder"; anchor: string; before: boolean }
  | { kind: "send_to_team"; team: string };
export type MenuAction = "to_backlog" | "to_todo" | "back_to_todo" | "accept" | "top";
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

/** The answer of Send to team: sent, or not with the reason as it came (a
 * refusal, or an unconfirmed write that may have happened). */
export interface SendOutcome { sent: boolean; reason?: string }

/** Sends a card in Backlog or To do to a team's queue (`task target REF team
 * TEAM`). The task board does not decide the outcome: a refusal is the team's
 * own reason, and a task already queued is not sent twice. `read` is a row the
 * caller read exactly (the New task recovery); the board's own row wins when
 * it has one. With no row at all the task is not read yet, which is not a
 * change. A `quiet` caller owns the presentation: nothing is notified. */
export async function sendToTeam(project: string, task: Pick<Task, "uuid">, team: string, teamName: string, options: { read?: Task; quiet?: boolean } = {}): Promise<SendOutcome> {
  const tell = (tone: "info" | "error", text: string) => { if (!options.quiet) notify(tone, text); };
  if (writingTask()) {
    const reason = tr("Another task change is being written. Nothing was sent.");
    tell("error", reason); return { sent: false, reason };
  }
  const board = tasks.project() === project ? tasks.find(task.uuid) : undefined;
  const fresh = board ?? (options.read?.uuid === task.uuid ? options.read : undefined);
  if (!fresh) {
    const reason = tr("The task is not read yet. Nothing was sent.");
    tell("error", reason); return { sent: false, reason };
  }
  if (!fresh.writable || (fresh.stage !== "backlog" && fresh.stage !== "todo")) {
    const reason = tr("The task changed. Nothing was changed. Refresh the task.");
    tell("error", reason); return { sent: false, reason };
  }
  setWritingTask(task.uuid);
  try {
    await taskBoardAction(project, task.uuid, fresh.stage, { kind: "send_to_team", team });
    tell("info", tr("Sent to %1", teamName));
    return { sent: true };
  } catch (error) {
    tell("error", String(error));
    return { sent: false, reason: String(error) };
  } finally { setWritingTask(undefined); }
}

export async function actOnTask(project: string, task: Task, action: BoardAction) {
  if (writingTask() || action.kind === "send_to_team") return;
  const fresh = tasks.project() === project ? tasks.find(task.uuid) : undefined;
  const anchor = action.kind === "reorder" ? tasks.find(action.anchor) : undefined;
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
