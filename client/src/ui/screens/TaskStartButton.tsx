import { taskStartBlocker, startTask } from "../../store/taskStart";
import { writingTask } from "../../store/taskBoard";
import { openTask } from "../../store/taskNavigation";
import type { Task } from "./tasks-ui";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

export function TaskStartButton(props: { project: string; task: Task; card?: boolean; showReason: (el: Element, text: string) => void }) {
  const why = () => taskStartBlocker(props.project, props.task);
  return <button class={props.card ? "icon-btn tk-act tk-primary" : "btn small tv-cmd is-primary"} classList={{ blocked: !!why() }} data-cmd="start"
    aria-label={tr("Start task")} aria-disabled={!!why()} aria-description={why()} title={why() ?? tr("Start task")} disabled={!!writingTask()}
    onClick={(e) => { e.stopPropagation(); const reason = why(); if (reason) { openTask(props.project, props.task.uuid); props.showReason(e.currentTarget, reason); } else void startTask(props.project, props.task); }}>
    <Icon name="play" size={props.card ? 15 : 13} />{props.card ? "" : tr("Start task")}
  </button>;
}
