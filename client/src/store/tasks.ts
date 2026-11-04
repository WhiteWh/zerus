import { createSignal } from "solid-js";
import { followTasks } from "../bridge";
import type { TasksOut } from "../types";
import { TASK_LIMIT, object, taskItem, text, type Task } from "../ui/screens/tasks-ui";
import { tr } from "../i18n";

/** One project list, replaced atomically by the existing event stream. */
export function createTasks(follow: (project: string | null) => Promise<void>) {
  const [project, setProject] = createSignal<string>();
  const [tasks, setTasks] = createSignal<Task[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [truncated, setTruncated] = createSignal(false);
  const [damaged, setDamaged] = createSignal(0);
  let sent = Promise.resolve(), generation = 0;
  return {
    project, tasks, loaded, error, truncated, damaged,
    follow(next: string | undefined) {
      if (next === project()) return;
      const mine = ++generation;
      setProject(next); setTasks([]); setLoaded(false); setError(""); setTruncated(false); setDamaged(0);
      sent = sent.then(() => follow(next ?? null)).catch((e) => { if (mine === generation) { setError(String(e)); setLoaded(true); } });
    },
    apply(updates: TasksOut[]) {
      for (const update of updates) {
        if (update.computer !== "" || update.key !== project()) continue;
        const p = object(update.payload);
        if (update.kind === "list") {
          if (text(object(p.project).id) !== project() || !Array.isArray(p.tasks)) continue;
          const seen = new Set<string>(); let queue = 0;
          const rows = p.tasks.slice(0, TASK_LIMIT).flatMap((value) => {
            const t = taskItem(value); if (!t.uuid || seen.has(t.uuid)) return [];
            seen.add(t.uuid); if (t.stage === "todo") t.queue = ++queue;
            return [t];
          });
          setTasks(rows); setError(""); setLoaded(true);
          setTruncated(p.truncated === true || p.tasks.length > TASK_LIMIT);
          setDamaged(Array.isArray(p.damaged) ? p.damaged.length : 0);
        } else if (update.kind === "failed") { setError(text(p.message) || tr("Tasks could not be read.")); setLoaded(true); }
        else if (update.kind === "ended") { setError(tr("The task subscription ended. Reopen Tasks to reconnect.")); setLoaded(true); }
      }
    },
  };
}
export const tasks = createTasks((project) => followTasks(project));
