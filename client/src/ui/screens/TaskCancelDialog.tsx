import { Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { taskCancellation, setTaskCancellation, confirmTaskCancellation } from "../../store/taskCancel";
import { orderedRows, computerOffline } from "../../store/catalog";
import { taskSessions, sessionLabel } from "./tasks-ui";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

/** The approved TaskDialogs Cancel dialog. Escape and Keep running never write. */
export default function TaskCancelDialog() {
  let el!: HTMLDialogElement, keep!: HTMLButtonElement;
  const pending = taskCancellation()!;
  const [reason, setReason] = createSignal("");
  const find = createMemo(() => taskSessions(orderedRows()));
  const session = () => find()(pending.task.executor);
  const running = () => { const s = session(); return !!s && !computerOffline(s.computer) && s.state === "running" && s.processState !== "exited"; };
  const working = () => session()?.status === "working";
  const worker = () => pending.task.worker || sessionLabel(pending.task.executor) || "its session";
  const close = () => { if (taskCancellation() === pending) setTaskCancellation(undefined); };
  onMount(() => { el.showModal(); queueMicrotask(() => keep?.focus()); });
  onCleanup(() => { if (el.open) el.close(); queueMicrotask(() => { if (!taskCancellation() && pending.origin?.isConnected) pending.origin.focus(); }); });
  return <dialog ref={el} class="dlg" role="alertdialog" aria-labelledby="task-cancel-title" aria-describedby="task-cancel-body" style={{ width: "500px" }} onCancel={close} onClose={close}>
    <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="task-cancel-title">{tr("Cancel %1?", pending.task.id)}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={close}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body td-form" id="task-cancel-body">
      <p class="td-cancel-text">{pending.task.text || pending.task.title}</p>
      <p><b>{pending.task.id}</b> {tr("moves to In review, and")} <b>{worker()}</b> {tr("gets the cancel message.")}
        <Show when={!running()}> {tr("The agent is not running, so hgs will refuse the message; the task is cancelled all the same.")}</Show>
        <Show when={running() && working()}> {worker()} {tr("is working; it reads the message when its turn ends.")}</Show>
      </p>
      <label class="td-label" for="td-reason">{tr("Reason (optional) — saved as a comment on the task, not sent to the agent")}</label>
      <input id="td-reason" class="field" placeholder={tr("Why it is cancelled")} value={reason()} onInput={(e) => setReason(e.currentTarget.value)} />
    </div>
    <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn solid-danger" onClick={() => void confirmTaskCancellation(reason())}><Icon name="x" size={13} />{tr("Cancel task")}</button><button ref={keep} type="button" class="btn primary" autofocus onClick={close}>{tr("Keep running")}</button></div>
  </dialog>;
}
