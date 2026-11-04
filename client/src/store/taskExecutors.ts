import { createSignal } from "solid-js";
import { taskExecutorOpen, taskExecutorRelease } from "../bridge";
import { loadNewSession } from "../ui/sessions/newsession-view";
import { notify } from "./actions";
import { toast } from "./notices";
import { writingTask, setWritingTask } from "./taskBoard";
import type { StartInput } from "./taskStart";
import type { Task } from "../ui/screens/tasks-ui";
import { tr } from "../i18n";
export type TaskLaunchContext = StartInput;
export interface ExecutorView { project: string; task: string; id: string; session: string; phase: string; snapshot: string; error: string }
export function createTaskExecutors(read: typeof taskExecutorOpen) {
  const [view, setView] = createSignal<ExecutorView>(), [error, setError] = createSignal("");
  let wanted: { project: string; task: string } | undefined, generation = 0, reading = false;
  const load = async () => {
    if (!wanted || reading) return;
    const target = wanted, mine = generation; reading = true;
    try { const answer = await read(target.project, target.task); if (mine === generation) { if (answer && (answer.project !== target.project || answer.task !== target.task)) throw new Error("The executor launch did not match the selected task."); setView(answer ?? undefined); setError(""); } }
    catch (e) { if (mine === generation) setError(String(e)); }
    finally { reading = false; if (mine !== generation && wanted) void load(); }
  };
  return { view, error, follow: (project?: string, task?: string) => { if (project === wanted?.project && task === wanted?.task) return; wanted = project && task ? { project, task } : undefined; generation++; setView(undefined); setError(""); void load(); }, refresh: () => { generation++; void load(); },
    released: (id: string) => { if (view()?.id === id) { generation++; setView(undefined); setError(""); } },
  };
}
export const taskExecutors = createTaskExecutors((project, task) => taskExecutorOpen(project, task));
export const pendingExecutor = (project: string, task: string) => { const view = taskExecutors.view(); return view?.project === project && view.task === task ? view : undefined; };
export const [taskExecutorReview, setTaskExecutorReview] = createSignal<{ task: Task; view: ExecutorView; origin: HTMLElement }>();
export async function configureTaskExecutor(input: TaskLaunchContext, agent: string) {
  const store = await loadNewSession();
  await store.openNewSession({ project: input.project, computer: input.target.computer, agent, task: input });
}
export async function releaseTaskExecutor(review: NonNullable<ReturnType<typeof taskExecutorReview>>) {
  if (writingTask() || review.view.phase === "waiting") return false;
  setWritingTask(review.task.uuid);
  try { await taskExecutorRelease(review.view.id, review.view.snapshot); taskExecutors.released(review.view.id); taskExecutors.refresh(); toast(tr("Launch record released"), tr("Another launch is allowed. Nothing was stopped or sent again.")); return true; }
  catch { notify("error", tr("Could not release the executor launch record."), true); return false; }
  finally { setWritingTask(undefined); }
}
