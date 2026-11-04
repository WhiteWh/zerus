import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { taskSettings } from "../../store/taskSettings";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

/** The approved TaskDialogs task-settings layout; data is read only on open. */
export default function TaskSettingsDialog() {
  const current = taskSettings.dialog()!;
  const [task, setTask] = createSignal(current.base.task_template), [cancel, setCancel] = createSignal(current.base.cancel_template);
  const [others, setOthers] = createSignal(current.base.agents_other), [limit, setLimit] = createSignal(Math.min(1000, current.base.agent_limit));
  let dialog!: HTMLDialogElement, first!: HTMLTextAreaElement;
  const close = () => taskSettings.close(current);
  const save = () => void taskSettings.save(current, { task_template: task(), cancel_template: cancel(), agents_other: others(), agent_limit: limit() });
  onMount(() => { dialog.showModal(); queueMicrotask(() => first?.focus()); });
  onCleanup(() => { if (dialog.open) dialog.close(); queueMicrotask(() => { if (!taskSettings.dialog() && current.origin?.isConnected) current.origin.focus(); }); });
  return <dialog ref={dialog} class="dlg" aria-labelledby="task-settings-title" style={{ width: "620px" }} onCancel={close} onClose={close}>
    <div class="dlg-head"><Icon name="settings" size={18} class="accent" /><h2 id="task-settings-title">{tr("Project tasks: %1", current.name || tr("Project"))}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={close}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body"><div class="td-form">
      <h3 class="td-heading">{tr("Service prompts")}</h3>
      <label class="td-label" for="ts-task">{tr("Task (Send now)")}</label>
      <textarea ref={first} id="ts-task" class="field mono td-template" rows={8} value={task()} onInput={(e) => setTask(e.currentTarget.value)} spellcheck={false} />
      <label class="td-label" for="ts-cancel">{tr("Cancel message")}</label>
      <textarea id="ts-cancel" class="field mono td-template" rows={4} value={cancel()} onInput={(e) => setCancel(e.currentTarget.value)} spellcheck={false} />
      <div class="hint">{tr("Placeholders:")} <code>{"{ref}"}</code>, <code>{"{author}"}</code>, <code>{"{prompt}"}</code>, <code>{"{tools}"}</code>, <code>{"{history}"}</code></div>
      <h3 class="td-heading">{tr("Agents")}</h3>
      <label class="check"><input type="checkbox" checked={others()} onChange={(e) => setOthers(e.currentTarget.checked)} />{tr("Agents may start and cancel tasks in other sessions")}</label>
      <div class="row td-limit"><label for="ts-limit">{tr("Limit tasks started by agents")}</label><input id="ts-limit" class="field tabular" type="number" min={0} max={1000} step={1} value={limit()} onInput={(e) => setLimit(e.currentTarget.valueAsNumber)} /><span class="muted">{tr("per hour")}</span><Show when={limit() === 0}><span class="chip">{tr("no limit")}</span></Show></div>
      <Show when={taskSettings.error()}><div class="tv-note-error" role="alert">{taskSettings.error()}</div></Show>
    </div></div>
    <div class="dlg-foot"><span class="spacer" /><button class="btn" onClick={close}>{tr("Cancel")}</button><button class="btn primary" disabled={taskSettings.busy()} onClick={save}>{tr("Save")}</button></div>
  </dialog>;
}
