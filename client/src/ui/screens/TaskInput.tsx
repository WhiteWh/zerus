import { descriptionEditing, setDescriptionEditing } from "../../store/taskStart";
import { noteOnly, setNoteOnly, queuedTask, messageChoice, messageLabel, messagePlaceholder, messageDelivery } from "../../store/taskMessage";
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { taskEditor, type TaskEditorMode, type TaskRecoveryRecord } from "../../store/taskEditor";
import TaskRecoveryRelease from "./TaskRecoveryRelease";
import { onCommand } from "../../store/shortcuts";
import { setMenu } from "../../store/ui";
import { openTaskAttachment, attachmentPin, attachmentUnpin } from "../../bridge";
import { notify } from "../../store/actions";
import { toast } from "../../store/notices";
import type { Task } from "./tasks-ui";
import { tr } from "../../i18n";

export function TaskInput(props: { project: string; task: Task; describing: boolean; cancel: () => void; saved: () => void }) {
  const owner = Symbol();
  createEffect(() => setDescriptionEditing(props.describing ? { project: props.project, task: props.task.uuid, owner } : undefined));
  onCleanup(() => { if (descriptionEditing()?.owner === owner) setDescriptionEditing(undefined); });
  let alive = true; onCleanup(() => { alive = false; });
  let editor!: HTMLTextAreaElement;
  const mode = (): TaskEditorMode => props.describing ? "description" : "note";
  const identity = createMemo(() => JSON.stringify([props.project, props.task.uuid, mode()]));
  const [releaseReview, setReleaseReview] = createSignal<{ handle: number; record: TaskRecoveryRecord; origin: HTMLElement }>();
  createEffect(on(identity, () => setReleaseReview(undefined)));
  createEffect(on(identity, () => taskEditor.follow({ project: props.project, task: props.task.uuid, mode: mode() })));
  onCleanup(() => taskEditor.follow());
  const view = () => { const v = taskEditor.view(); return v?.project === props.project && v.task === props.task.uuid && v.mode === mode() ? v : undefined; };
  const locked = () => !props.task.writable || !view() || taskEditor.locked();
  const text = () => view()?.text ?? "";
  const files = () => view()?.files ?? [];
  const label = () => props.describing ? tr("Save description") : noteOnly() || queuedTask(props.task) ? tr("Add to task") : tr("Send feedback");
  const save = async (recover = false) => { if (locked()) return; const delivery = !recover && !props.describing ? messageDelivery(props.task, noteOnly()) : undefined; if (await taskEditor.submit(recover, delivery) && alive) { props.saved(); queueMicrotask(() => editor.focus()); } };
  const recoveryMenu = (origin: HTMLButtonElement, point?: { x: number; y: number }) => {
    if (locked()) return;
    const current = view(), record = current?.recovery_record, r = origin.getBoundingClientRect();
    setMenu({ x: point?.x ?? r.left, y: point?.y ?? r.bottom + 4, items: [
      { label: tr("Check delivery receipt"), icon: "refresh-cw", action: () => void save(true) },
      { label: tr("Release local recovery record…"), icon: "trash", disabled: !record, action: () => { if (record && current?.handle === view()?.handle) setReleaseReview({ handle: current.handle, record, origin }); } },
    ] });
  };
  const release = () => {
    const review = releaseReview(); if (!review) return;
    setReleaseReview(undefined);
    void taskEditor.release(review.handle, review.record).then((released) => { if (released) { toast(tr("Recovery record released"), tr("The comment stays in the task; nothing was cancelled or sent again.")); if (alive && view()?.handle === review.handle) queueMicrotask(() => editor.focus()); } });
  };
  onMount(() => { onCleanup(onCommand("message.send", () => void save(), editor)); });
  const cursor = () => {
    const position = editor.selectionDirection === "backward" ? editor.selectionStart : editor.selectionEnd;
    const anchor = editor.selectionDirection === "backward" ? editor.selectionEnd : editor.selectionStart;
    taskEditor.edit(editor.value, position, anchor);
  };
  createEffect(on(() => [view()?.handle, view()?.files] as const, () => {
    const v = view(); if (!v) return;
    queueMicrotask(() => { editor.setSelectionRange(Math.min(v.position, v.anchor), Math.max(v.position, v.anchor), v.position < v.anchor ? "backward" : "forward"); if (props.describing) editor.focus(); });
  }));
  const fileMenu = (button: HTMLButtonElement) => {
    const r = button.getBoundingClientRect();
    setMenu({ x: r.left, y: r.top, items: files().flatMap((f, index) => [
      { label: `Open ${f.name}`, icon: "external-link", action: () => { void openTaskAttachment(f.path).then((message) => { if (message) notify("info", message); }).catch((e) => notify("error", String(e))); } },
      { label: `Remove ${f.name}`, icon: "x", disabled: locked(), action: () => { void taskEditor.remove(index); } },
    ]) });
  };
  createEffect(() => { const paths = files().map((f) => f.path); paths.forEach(attachmentPin); onCleanup(() => paths.forEach(attachmentUnpin)); });
  return <div class="cmp-col"><div class="tv-composer-wrap"><div class="composer tv-composer" classList={{ disabled: locked(), describing: props.describing }}>
    <div class="cmp-toolbar"><Show when={props.describing} fallback={<button class="tchip tv-mode" disabled={locked() || !!view()?.recovery} title={messageLabel(props.task, noteOnly())} aria-haspopup="menu" onClick={(e) => { const r = e.currentTarget.getBoundingClientRect(); setMenu({ x: r.left, y: r.bottom + 4, items: [{ label: messageChoice(props.task), checked: !noteOnly(), action: () => setNoteOnly(false) }, { label: tr("Save note only"), checked: noteOnly(), action: () => setNoteOnly(true) }] }); }}><Icon name={noteOnly() ? "file-text" : queuedTask(props.task) ? "clock" : "send"} size={12} /><span class="ell">{messageLabel(props.task, noteOnly())}</span><Icon name="chevron-down" size={12} /></button>}>
      <span class="tchip accent tv-mode" title={tr("Save updates the canonical task document. Comments remain separate.")}><Icon name="pencil" size={12} />{tr("Editing description")}</span><button class="tchip" disabled={taskEditor.busy()} onClick={props.cancel}>{tr("Cancel")}</button>
    </Show><Show when={files().length}><button class="tchip" title={tr("Attachments: preview, open or remove")} onClick={(e) => fileMenu(e.currentTarget)}><Icon name="paperclip" size={12} />{files().length} attached<Icon name="chevron-down" size={12} /></button></Show>
      <Show when={view()?.recovery && !props.describing}><button class="tchip warn" title={tr("Keep the previous comment in the task and check its delivery. It will not be sent again.")} disabled={locked()} onClick={() => void save(true)} onContextMenu={(e) => { e.preventDefault(); recoveryMenu(e.currentTarget, { x: e.clientX, y: e.clientY }); }} onKeyDown={(e) => { if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) { e.preventDefault(); recoveryMenu(e.currentTarget); } }}><Icon name="rotate-ccw" size={12} />{tr("Recover saved comment")}</button></Show>
      <span class="grow" /><button class="tchip icon" title={tr("Jump to the latest discussion")} onClick={() => { const scroll = document.querySelector(".tv-scroll"); scroll?.scrollTo({ top: scroll.scrollHeight, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" }); }}><Icon name="chevrons-down" size={13} /></button>
    </div>
    <textarea ref={editor} class="cmp-editor" aria-label={props.describing ? tr("Task description") : tr("Comment on this task")} placeholder={!props.task.writable ? tr("Update Zerus to comment on this task") : props.describing ? tr("Describe the task…") : messagePlaceholder(props.task, noteOnly())} value={text()} readOnly={locked()} onInput={cursor}
      onSelect={() => { const v = view(); if (v && document.activeElement === editor && (v.position !== editor.selectionEnd || v.anchor !== editor.selectionStart)) cursor(); }} onBlur={() => void taskEditor.flush().catch(() => {})}
      onKeyDown={(e) => { if (!e.defaultPrevented && e.key === "Enter" && !e.shiftKey && !e.isComposing && e.keyCode !== 229) { e.preventDefault(); void save(); } }} />
    <div class="cmp-actions"><button class="icon-btn cmp-attach" aria-label={tr("Attach files")} title={tr("Attach files (up to 8 files, 10 MiB each, 20 MiB per task)")} disabled={locked()} onClick={() => void taskEditor.pick().then(() => { if (alive) editor.focus(); })}><Icon name="paperclip" size={17} /></button><span class="cmp-status ell">{props.describing ? tr("Save updates the canonical task document. Comments remain separate.") : ""}</span><span class="grow" />
      <Show when={props.describing} fallback={<button class="btn primary tv-send" title={label()} aria-label={label()} disabled={locked() || (!text().trim() && !files().length) || !!view()?.recovery} onClick={() => void save()}><Icon name={taskEditor.busy() ? "loader-circle" : noteOnly() || queuedTask(props.task) ? "plus" : "send"} size={14} /></button>}><button class="btn primary" disabled={locked()} onClick={() => void save()}>{tr("Save description")}</button></Show>
    </div>
  </div><Show when={taskEditor.error()}><div class="tv-note-error" role="alert">{taskEditor.error()}</div></Show>
    <Show when={props.describing && files().length}><div class="tv-draft-files"><For each={files()}>{(file) => <span class="att-chip"><Icon name={file.thumbnail ? "image" : "file-text"} size={13} />{file.reference} {file.name}</span>}</For></div></Show>
  </div><Show when={releaseReview()} keyed>{(review) => <TaskRecoveryRelease origin={review.origin} close={() => setReleaseReview(undefined)} release={release} />}</Show></div>;
}
