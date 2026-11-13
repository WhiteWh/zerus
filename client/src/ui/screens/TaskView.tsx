import { Show, createResource } from "solid-js";
import { TaskInspector } from "./TaskInspector";
import { taskProject as project, projectTasks as rows, findTaskSession as find } from "../../store/taskContext";
import { tasks } from "../../store/tasks";
import { selectedTask } from "../../store/taskNavigation";
import { setSelectedKey, setDetailTab } from "../../store/ui";
import { readSelectedTask } from "./task-view-read";
import { executorRef, projectTask } from "./tasks-ui";
import "../styles/tasks.css";
import { tr } from "../../i18n";

export default function TaskView() {
  const wanted = () => { const id = selectedTask(); return id && id.project === project()?.id ? id : undefined; };
  const listed = () => { const id = wanted(); return id ? rows().find((t) => t.uuid === id.uuid) : undefined; };
  // A windowed list loads only some rows of each stage: a selected task beyond them is read on its own.
  const missing = () => { const id = wanted(); return id && tasks.loaded() && !listed() ? `${id.project}\n${id.uuid}` : undefined; };
  const [read] = createResource(missing, async (key) => { const task = await readSelectedTask(key); tasks.adopt(task); return task; });
  // Loading first: a pending read() would suspend this view's boundary and hide
  // the Loading task… status below until the task arrives.
  const fetched = () => missing() && !read.loading && !read.error ? read() : undefined;
  const chosen = () => listed() ?? (fetched() ? projectTask(fetched()!, find()) : undefined);
  const raw = () => tasks.tasks().find((t) => t.uuid === wanted()?.uuid) ?? fetched();
  const empty = () => !wanted()
    ? <>{tr("Select a task in the Tasks panel to see its details.")}<br />{tr("Open the side panel's Tasks tab and click a card.")}</>
    : read.error ? tr("This task could not be read.") : tr("Loading task…");
  return <div class="tv"><Show when={chosen()} fallback={<p class="tasks-empty" role="status">{empty()}</p>}>{(task) => <TaskInspector project={project()!.id} task={raw() ?? task()} presentation={task()} executorAvailable={!!find()(executorRef(task()))} openExecutor={() => { const s = find()(executorRef(task())); if (s) { setSelectedKey(s.key); setDetailTab("activity"); } }} />}</Show></div>;
}
