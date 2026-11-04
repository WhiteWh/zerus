import { Show } from "solid-js";
import { TaskInspector } from "./TaskInspector";
import { taskProject as project, projectTasks as rows, findTaskSession as find } from "../../store/taskContext";
import { tasks } from "../../store/tasks";
import { selectedTask } from "../../store/taskNavigation";
import { setSelectedKey, setDetailTab } from "../../store/ui";
import { executorRef } from "./tasks-ui";
import "../styles/tasks.css";
import { tr } from "../../i18n";

export default function TaskView() {
  const chosen = () => selectedTask()?.project === project()?.id ? rows().find((t) => t.uuid === selectedTask()?.uuid) : undefined;
  return <div class="tv"><Show when={chosen()} fallback={<p class="tasks-empty">{tr("Select a task in the Tasks panel to see its details.")}<br />{tr("Open the side panel's Tasks tab and click a card.")}</p>}>{(task) => <TaskInspector project={project()!.id} task={tasks.tasks().find((t) => t.uuid === task().uuid)!} presentation={task()} executorAvailable={!!find()(executorRef(task()))} openExecutor={() => { const s = find()(executorRef(task())); if (s) { setSelectedKey(s.key); setDetailTab("activity"); } }} />}</Show></div>;
}
