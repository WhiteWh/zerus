import { selected as selectedSession } from "../../store/ui";
import { writingTask } from "../../store/taskBoard";
import { openTaskAssignment } from "../../store/taskAssignment";
import { pendingExecutor, taskExecutors, setTaskExecutorReview } from "../../store/taskExecutors";
import { onTaskExecutors } from "../../bridge";
import { taskStarts, supportsTaskStart, taskStartBlocker, pendingStart, reviewTaskStart } from "../../store/taskStart";
import { PAUSE_REASON } from "../../store/taskCancel";
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup } from "solid-js";
import { convertFileSrc } from "@tauri-apps/api/core";
import { attachmentPin, attachmentUnpin, inTauri, openTaskAttachment } from "../../bridge";
import { notify } from "../../store/actions";
import { taskDetails, type TaskEntry } from "../../store/taskDetails";
import type { Fetched } from "../../types";
import { TaskWorkflow } from "./TaskWorkflow";
import { TaskTitle } from "./TaskTitle";
import { taskRenameRequest, setTaskRenameRequest, taskDescriptionRequest, setTaskDescriptionRequest } from "../../store/taskNavigation";
import { TaskInput } from "./TaskInput";
import { setMenu } from "../../store/ui";
import { Icon } from "../lib/icons";
import { Body } from "../sessions/Activity";
import { messageTimeOf } from "../lib/dates";
import { assignmentText, assignmentLabel, STAGE_LABEL, stageLabel, type Stage, type Task } from "./tasks-ui";
import { deliveryError, deliveryText, historyAction } from "./task-history-ui";
import { tr } from "../../i18n";

function TaskFiles(props: { files: Fetched[] }) {
  return <div class="tasks-files"><For each={props.files}>{(file) => {
    attachmentPin(file.path); onCleanup(() => attachmentUnpin(file.path));
    const open = async () => {
      try { const notice = await openTaskAttachment(file.path); if (notice) notify("info", notice); }
      catch (error) { notify("error", String(error)); }
    };
    return <button class="att-card" onClick={() => void open()} title={`Open ${file.name}`}>
      <Show when={file.thumbnail && inTauri()}><img class="tasks-file-preview" src={convertFileSrc(file.path, "zerus-file")} alt="" /></Show>
      <span>{file.name}</span><span class="muted">{Math.max(1, Math.ceil(file.bytes / 1024))} KiB</span><span>{tr("Open →")}</span>
    </button>;
  }}</For></div>;
}

function JournalEntry(props: { entry: TaskEntry; discussion?: boolean; worker: string }) {
  const message = () => ["comment", "message", "report"].includes(props.entry.kind);
  const label = () => props.entry.kind === "report" ? tr("Report") : !message() ? "" : !props.discussion ? tr("Comment") : props.entry.kind === "message" ? tr("Message") : tr("Note");
  const date = () => { const d = new Date(props.entry.at); return props.entry.at > 0 && Number.isFinite(d.getTime()) ? d : undefined; };
  if (!["message", "comment", "report"].includes(props.entry.kind)) return <div class="tv-event"><i aria-hidden="true" /><span><b>{props.entry.actor}</b> <time>{date() && messageTimeOf(date()!)}</time> {historyAction(props.entry)}</span></div>;
  return <article class="act-msg tv-entry" classList={{ "k-user": props.entry.actor === "You", "k-agent": props.entry.actor !== "You" }}>
    <header><b>{props.entry.actor}</b><Show when={date()}>{(d) => <time datetime={d().toISOString()}>{messageTimeOf(d())}</time>}</Show>
      <span class="chip">{label()}</span></header>
    <div class="act-body"><Body text={message() ? props.entry.text : historyAction(props.entry)} /></div>
    <TaskFiles files={props.entry.files} />
    <Show when={props.discussion}><span class="tv-delivery">{deliveryText(props.entry, props.worker)}</span><Show when={deliveryError(props.entry)}><p class="notice">{deliveryError(props.entry)}</p></Show></Show>
  </article>;
}

const TABS = ["Task", "Result", "History"] as const;
/** Qt's canonical document and result beside a bounded discussion/history. */
export function TaskInspector(props: { project: string; task: Task; presentation?: Task; executorAvailable: boolean; openExecutor: () => void }) {
  const [describing, setDescribing] = createSignal(false), [renaming, setRenaming] = createSignal(false);
  const [tab, setTab] = createSignal<typeof TABS[number]>("Task"), [count, setCount] = createSignal(8);
  const identity = createMemo(() => `${props.project}\n${props.task.uuid}`);
  const revision = createMemo(() => JSON.stringify([props.project, props.task.uuid, props.task.historyRevision || props.task]));
  let selected = "";
  createEffect(on(revision, () => {
    const task = props.task, key = identity();
    if (key !== selected) { selected = key; taskDetails.select(props.project, task.uuid); }
    else taskDetails.refresh();
  }));
  createEffect(on(identity, () => { setTab("Task"); setCount(8); setDescribing(false); setRenaming(false); }));
  createEffect(() => { const request = taskRenameRequest(); if (request?.project === props.project && request.uuid === props.task.uuid) { setTaskRenameRequest(undefined); setRenaming(true); } });
  createEffect(() => { const request = taskDescriptionRequest(); if (request?.project === props.project && request.uuid === props.task.uuid) { setTaskDescriptionRequest(undefined); if (["todo", "backlog"].includes(props.task.stage)) setDescribing(true); } });
  createEffect(on(identity, () => { taskStarts.follow(props.project, props.task.uuid); taskExecutors.follow(props.project, props.task.uuid); }));
  let executorUnlisten: (() => void) | undefined, executorClosed = false;
  void onTaskExecutors(() => taskExecutors.refresh()).then((stop) => { if (executorClosed) stop(); else { executorUnlisten = stop; taskExecutors.refresh(); } }).catch(() => {});
  onCleanup(() => { executorClosed = true; executorUnlisten?.(); taskExecutors.follow(); });
  onCleanup(() => { taskDetails.select(); taskStarts.follow(); });
  const detail = taskDetails.data;
  const item = () => props.presentation ?? props.task;
  const shown = () => detail()?.log.slice(-count()) ?? [];
  const discussion = () => detail()?.log.filter((e) => e.kind === "message" || e.kind === "comment") ?? [];
  return <>
    <div class="tv-col"><header class="tv-head"><div class="tv-title-row"><TaskTitle project={props.project} task={item()} title={detail()?.title || item().title || tr("Untitled task")} renaming={renaming()} setRenaming={setRenaming} saved={taskDetails.refresh} /><button class="icon-btn tv-more" aria-label={tr("Task actions")} title={tr("Task actions")} aria-haspopup="menu" onClick={(e) => { const box = e.currentTarget.getBoundingClientRect(); setMenu({ x: box.left, y: box.bottom, items: [{ label: tr("Rename task"), icon: "pencil", disabled: !item().writable, action: (): void => { setRenaming(true); } }, { separator: true }, ...TABS.map((name) => ({ label: name === "Task" ? tr("Task and discussion") : name, checked: tab() === name, action: () => setTab(name) }))] }); }}><Icon name="ellipsis" size={17} /></button></div>
      <div class="tv-context"><span class={`tv-stage st-${item().stage}`}><span class={`tk-glyph st-${item().stage}`} /><span>{stageLabel(item().stage)}</span></span><span class="mono tv-id">{item().id}</span><button class="tchip tv-assign" aria-label={tr("Task executor")} disabled={["todo", "backlog"].includes(item().stage) ? !item().writable || !!writingTask() : !props.executorAvailable} onClick={(e) => { if (["todo", "backlog"].includes(item().stage)) openTaskAssignment(props.project, item(), e.currentTarget); else props.openExecutor(); }} title={`${assignmentText(item(), selectedSession())}\n${["todo", "backlog"].includes(item().stage) ? tr("Change executor assignment") : tr("Open executor Activity")}`}><Icon name={["todo", "backlog"].includes(item().stage) ? "user-round-cog" : "bot"} size={12} /><span class="ell">{assignmentText(item(), selectedSession())}</span><Icon name={["todo", "backlog"].includes(item().stage) ? "chevron-down" : "arrow-up-right"} size={12} /></button><span class="grow" /><TaskWorkflow project={props.project} task={item()} /></div>
      <Show when={supportsTaskStart(item()) && ["todo", "backlog"].includes(item().stage) && taskStartBlocker(props.project, item())}>{(reason) => <div class="tv-blocker"><Icon name="triangle-alert" size={13} /><span>Cannot start: {reason()}</span><span class="grow" /><Show when={pendingExecutor(props.project, item().uuid)}>{(launch) => <button class="btn small" onClick={(e) => setTaskExecutorReview({ task: { ...item() }, view: { ...launch() }, origin: e.currentTarget })}><Icon name="eye" size={12} />{tr("Review executor launch")}</button>}</Show><Show when={pendingStart(props.project, item())}><button class="btn small" onClick={(e) => reviewTaskStart(props.project, item(), e.currentTarget)}><Icon name="eye" size={12} />{tr("Review task start")}</button></Show></div>}</Show>
      <Show when={item().stage === "active"}><div class="tv-blocker quiet"><Icon name="info" size={13} /><span>{tr(PAUSE_REASON)}</span></div></Show>
    </header></div><div class="tv-scroll"><div class="tv-col tv-content">
    <Show when={tab() !== "Task"}><div class="tv-section"><span class="eyebrow">{tab()}</span><span class="grow" /><button class="link tv-back" onClick={() => setTab("Task")}><Icon name="chevron-left" size={12} />{tr("Task and discussion")}</button></div></Show>
    <Show when={taskDetails.error()}><p class="notice danger" role="alert">Task could not be read: {taskDetails.error()}</p><button class="btn small" disabled={taskDetails.loading()} onClick={taskDetails.refresh}>{tr("Refresh")}</button></Show>
    <Show when={detail()} fallback={<p class="tasks-empty">{taskDetails.loading() ? tr("Loading task…") : tr("Task details are unavailable.")}</p>}>{(d) => <>
      <Show when={d().stage === "active" && !d().started}><p class="notice">Dispatched to {assignmentLabel(d())} — start unconfirmed</p></Show>
      <Show when={tab() === "Task"}>
        <Show when={(d().stage === "review" || d().stage === "done") && (d().summary || d().cancelReason)}><article class="act-msg tv-entry tv-result k-agent"><header><b>{item().worker || "Agent"}</b><span class="chip">{d().cancelledAt ? tr("Cancelled") : tr("Result")}</span></header><div class="act-body"><Body text={d().cancelReason || d().summary} /></div></article></Show>
        <section class="tv-doc" classList={{ editing: describing() }} aria-label={tr("Description")}><header><span class="eyebrow">{tr("Description")}</span><span class="grow" /><Show when={d().writable && ["todo", "backlog"].includes(d().stage)}><Show when={describing()} fallback={<button class="btn small ghost" onClick={() => setDescribing(true)}><Icon name="pencil" size={12} />{tr("Edit description")}</button>}><span class="tv-doc-state"><Icon name="pencil" size={11} />{tr("Being edited below")}</span></Show></Show></header><div class="act-body"><Body text={d().text || tr("No description yet.")} /><TaskFiles files={d().files} /></div></section>
        <div class="tv-section"><span class="eyebrow">{tr("Discussion")}</span></div>
        <For each={discussion()}>{(entry) => <JournalEntry entry={entry} worker={d().worker} discussion />}</For>
        <Show when={!discussion().length}><div class="tv-placeholder">{tr("Messages and notes appear here.")}</div></Show>
        <Show when={d().total > d().log.length}><p class="muted">{tr("Showing recent discussion. Earlier records are available in History.")}</p></Show>
      </Show>
      <Show when={tab() === "Result"}><article class="tasks-document"><h3>{d().cancelledAt ? tr("Cancelled") : tr("Report")}</h3><Body text={d().cancelReason || d().summary || tr("No result reported yet.")} /></article></Show>
      <Show when={tab() === "History"}>
        <div class="tasks-navigation"><Show when={count() < d().log.length || d().next}><button class="btn small" disabled={taskDetails.loading()} onClick={() => {
          if (count() < d().log.length) setCount(count() + 8); else { setCount(8); taskDetails.older(); }
        }}>{count() < d().log.length ? tr("Earlier events (%1)", Math.max(0, d().log.length - count())) : tr("Earlier events…")}</button></Show>
          <span class="grow" /><Show when={taskDetails.before()}><button class="btn small" disabled={taskDetails.loading()} onClick={() => { setCount(8); taskDetails.latest(); }}>{tr("Latest")}</button></Show></div>
        <For each={shown()}>{(entry) => <JournalEntry entry={entry} worker={d().worker} />}</For>
        <Show when={!d().log.length}><p class="tasks-empty">{tr("No history yet.")}</p></Show>
      </Show>
    </>}</Show>
  </div></div><TaskInput project={props.project} task={item()} describing={describing()} cancel={() => setDescribing(false)} saved={() => { setDescribing(false); taskDetails.refresh(); }} /></>;
}
