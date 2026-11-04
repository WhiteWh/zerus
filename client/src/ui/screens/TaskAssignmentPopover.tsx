import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { assign, canAssign, taskAssignmentPopover, setTaskAssignmentPopover } from "../../store/taskAssignment";
import { projectSessions, taskProject } from "../../store/taskContext";
import { selected } from "../../store/ui";
import { sessionIdentity, sessionLabel } from "./tasks-ui";
import { statusOf } from "../sessions/SessionCard";
import { Icon } from "../lib/icons";
import type { Session } from "../sessions/types";
import { tr } from "../../i18n";

/** The approved Task executor menu, including its paged specific-session column. */
export default function TaskAssignmentPopover() {
  const request = taskAssignmentPopover()!, parent = selected() ? { ...selected()! } : undefined;
  const [sub, setSub] = createSignal(false), [query, setQuery] = createSignal(""), [page, setPage] = createSignal(0);
  let el!: HTMLDivElement, search!: HTMLInputElement, returnFocus = false;
  const close = (focus = true) => { returnFocus = focus; if (taskAssignmentPopover() === request) setTaskAssignmentPopover(undefined); };
  const choose = (row?: Session, mode: "session" | "subagent" | "new_session" = "session") => { void assign(request.project, request.task, row ? { ...row } : undefined, mode); close(); };
  const matches = createMemo(() => projectSessions().filter((s) => canAssign(request.task, request.project, s) && `${s.name} ${s.computer}`.toLowerCase().includes(query().toLowerCase())));
  const pages = () => Math.max(1, Math.ceil(matches().length / 50));
  const at = () => Math.min(page(), pages() - 1), shown = () => matches().slice(at() * 50, at() * 50 + 50);
  const parentLabel = () => parent ? sessionLabel({ host: parent.computer, name: parent.name, id: sessionIdentity(parent), known: true }) : tr("Any session");
  const current = (s: Session) => request.task.targetKind === "session" && request.task.target.host === s.computer && request.task.target.id === sessionIdentity(s);
  createEffect(() => { if (taskProject()?.id !== request.project || selected()?.key !== parent?.key) close(false); });
  createEffect(() => { sub(); const anchor = request.origin.getBoundingClientRect(), r = el.getBoundingClientRect(); el.style.left = `${Math.max(8, Math.min(anchor.left, innerWidth - r.width - 8))}px`; el.style.top = `${Math.max(8, Math.min(anchor.bottom + 4, innerHeight - r.height - 8))}px`; });
  createEffect(() => { if (sub()) queueMicrotask(() => search.focus()); });
  onMount(() => {
    el.querySelector<HTMLElement>("button:not(:disabled)")?.focus();
    const outside = (e: PointerEvent) => { if (!el.contains(e.target as Node)) close(false); };
    addEventListener("pointerdown", outside); onCleanup(() => removeEventListener("pointerdown", outside));
  });
  onCleanup(() => { if (returnFocus) queueMicrotask(() => { if (request.origin.isConnected) request.origin.focus(); }); });
  return <Portal><div ref={el} class="menu tk-pop tk-assign" role="menu" aria-label={tr("Task executor")} data-shortcut-context="none" onKeyDown={(e) => {
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); }
    if ((e.key === "ArrowDown" || e.key === "ArrowUp") && e.target !== search) { e.preventDefault(); const buttons = [...el.querySelectorAll<HTMLButtonElement>("button.menu-item:not(:disabled)")], i = buttons.indexOf(document.activeElement as HTMLButtonElement); buttons[(i + (e.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length]?.focus(); }
  }}><div class="tk-assign-cols"><div class="tk-assign-main">
    <button class="menu-item tk-mi" disabled={!parent || !canAssign(request.task, request.project, parent)} onClick={() => choose(parent)}><Icon name="monitor" size={14} /><span class="tk-mi-text"><span>Main session: {parentLabel()}</span><Show when={!parent}><small>{tr("Open the session to send this task to")}</small></Show></span></button>
    <div class="menu-label">{tr("New executor, created when the task starts")}</div>
    <button class="menu-item tk-mi" disabled={!parent || !canAssign(request.task, request.project, parent)} onClick={() => choose(parent, "subagent")}><Icon name="bot" size={14} /><span class="tk-mi-text">{tr("Subagent")}</span></button>
    <button class="menu-item tk-mi" disabled={!parent || !canAssign(request.task, request.project, parent)} onClick={() => choose(parent, "new_session")}><Icon name="plus" size={14} /><span class="tk-mi-text">{tr("New session")}</span></button>
    <div class="menu-sep" />
    <button class="menu-item tk-mi" onClick={() => choose()}><Icon name="users" size={14} /><span class="tk-mi-text">{tr("Any session (the one selected at start)")}</span></button>
    <button class="menu-item tk-mi" classList={{ open: sub() }} aria-haspopup="true" aria-expanded={sub()} onClick={() => setSub(!sub())} onKeyDown={(e) => { if (e.key === "ArrowRight") { e.preventDefault(); setSub(true); } }}><Icon name="list-filter" size={14} /><span class="tk-mi-text">{tr("Specific session")}</span><Icon name="chevron-right" size={13} class="tk-mi-end" /></button>
  </div><Show when={sub()}><div class="tk-assign-sub" role="group" aria-label={tr("Specific session")}><input ref={search} class="field tk-assign-search" placeholder={tr("Find a session…")} aria-label={tr("Find a session")} value={query()} onInput={(e) => { setQuery(e.currentTarget.value); setPage(0); }} onKeyDown={(e) => { if (e.key === "ArrowLeft" && !query()) { e.preventDefault(); setSub(false); el.querySelector<HTMLElement>("[aria-haspopup=true]")?.focus(); } }} /><div class="tk-assign-list"><For each={shown()} fallback={<div class="hint tk-assign-none">{tr("No session matches.")}</div>}>{(s) => <button class="menu-item tk-mi" role="menuitemradio" aria-checked={current(s)} onClick={() => choose(s)}><span class={`tk-dot k-${statusOf(s)}`} /><span class="tk-mi-text"><span class="ell">{sessionLabel({ host: s.computer, name: s.name, id: sessionIdentity(s), known: true })}</span></span><Show when={current(s)}><Icon name="check" size={13} class="tk-mi-end accent" /></Show></button>}</For></div><div class="tk-assign-nav"><button class="btn small" disabled={!at()} onClick={() => setPage(at() - 1)}>{tr("Previous")}</button><span class="dim tabular">{at() + 1} / {pages()}</span><button class="btn small" disabled={at() >= pages() - 1} onClick={() => setPage(at() + 1)}>{tr("Next")}</button></div></div></Show></div></div></Portal>;
}
