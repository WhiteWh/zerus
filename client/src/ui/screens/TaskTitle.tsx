import { Show, createEffect, on, onCleanup, onMount } from "solid-js";
import { taskTitle } from "../../store/taskForms";
import { onCommand } from "../../store/shortcuts";
import { STAGE_LABEL, stageLabel, type Stage, type Task } from "./tasks-ui";
import { tr } from "../../i18n";

export function TaskTitle(props: { project: string; task: Task; title: string; renaming: boolean; setRenaming: (value: boolean) => void; saved: () => void }) {
  let root!: HTMLDivElement, field!: HTMLTextAreaElement;
  const view = () => { const v = taskTitle.view(); return v?.project === props.project && v.task === props.task.uuid ? v : undefined; };
  const begin = () => { if (props.task.writable) props.setRenaming(true); };
  createEffect(on(() => [props.project, props.task.uuid, props.renaming] as const, () => taskTitle.follow(props.renaming ? { project: props.project, task: props.task.uuid, mode: "title" } : undefined)));
  onCleanup(() => taskTitle.follow());
  createEffect(on(() => view()?.handle, () => { if (props.renaming && view()) queueMicrotask(() => { field?.focus(); field?.select(); }); }));
  onMount(() => { const off = onCommand("session.rename", begin, root); onCleanup(off); });
  const finish = async () => { if (!props.renaming || taskTitle.locked()) return; if (await taskTitle.submit()) { props.setRenaming(false); props.saved(); } };
  const cancel = async () => { if (taskTitle.locked()) return; if (await taskTitle.discard()) props.setRenaming(false); };
  return <div ref={root} class="tv-title-col"><Show when={props.renaming} fallback={<h1 class="tv-title" classList={{ editable: props.task.writable }} tabindex={props.task.writable ? 0 : -1} title={`${props.task.id}\n${stageLabel(props.task.stage)}${props.task.writable ? "\nClick to rename" : ""}`} onClick={begin} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); begin(); } }}>{props.title}</h1>}>
    <textarea ref={field} class="field tv-title-edit" rows={2} maxlength={512} aria-label={tr("Task title (Enter saves, Escape cancels)")} data-shortcut-context="none" value={view()?.text ?? ""} readOnly={taskTitle.locked()} disabled={!!view()?.blocked}
      onInput={(e) => taskTitle.edit(e.currentTarget.value, e.currentTarget.selectionEnd, e.currentTarget.selectionStart)} onBlur={() => void finish()}
      onKeyDown={(e) => { if (e.isComposing) return; if (e.key === "Enter") { e.preventDefault(); void finish(); } else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); void cancel(); } }} />
  </Show><Show when={props.renaming && taskTitle.error()}><div class="tv-title-error" role="alert">{taskTitle.error()}</div></Show></div>;
}
