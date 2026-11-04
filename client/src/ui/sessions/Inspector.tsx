import { For, Show, Switch, Match, Suspense, lazy, createEffect, createMemo, createSignal, onCleanup, on, type JSX } from "solid-js";
import { Icon } from "../lib/icons";
import { selected, setSubagentOpen, setDetailTab, setSelectedKey, subagentOpen, setMenu } from "../../store/ui";
import { inspectorOpen, inspectorTab, inspectorWidth, inspectorClosed, setInspectorOpen, setInspectorTab, setInspectorWidth, toggleInspectorCard, panelGeometry, inspectorHint, INSPECTOR_TABS, type InspectorTab } from "../../store/inspector";
import { followedFor, detailsFor, copyIdentity, copy } from "../../store/actions";
import { newSessionFromWorktree } from "./newsession-view";
import { catalogStatus, catalogRoots } from "./worktree-dialogs";
import { openWorktreeReview, worktreeReviewDialog } from "../../store/worktreeCleanup";
import { computerOffline, orderedRows } from "../../store/catalog";
import { groupOf, grouping, INBOX } from "../../store/organization";
import { sessionIdentity, snapshotOf } from "./actions-ui";
import { tasks } from "../../store/tasks";
import { taskProject, projectTasks } from "../../store/taskContext";
import "../styles/tasks.css";
import { object, text } from "../screens/tasks-ui";
import { homeShort, sessionAccount, sessionGoal, sessionPlan, usageRows } from "./inspector-data";
import { machineCards } from "../../store/machines";
import { filterFolder, clearFolderFilter } from "../../store/folderFilter";
import { of, sameKey, native } from "./paths";
import type { BoundRow } from "./scope-filters";
import type { Session } from "./types";
import { captionText } from "./SessionCard";
import { tr, trNoop } from "../../i18n";
import { accountsPage } from "../../store/accounts";
import { openFilterAccounts } from "../../store/accountFilters";
import { sessionUsageView } from "./session-usage-ui";
import { composer } from "../../store/composer";
import * as bridge from "../../bridge";
import { eventTimeOf } from "../lib/dates";
import "../styles/inspector.css";

// Keep the always-visible badge eager; load the board only when its tab opens.
const TasksPanel = lazy(() => import("../screens/Tasks").then((module) => ({ default: module.TasksPanel })));
function TaskTabBadge() { return <Show when={taskProject() && tasks.loaded()}><span class="tk-tabbadge"><Show when={projectTasks().filter((t) => t.stage === "review").length}>{(n) => <span class="tk-tb review">{n()}</span>}</Show><Show when={projectTasks().filter((t) => t.stage === "todo").length}>{(n) => <span class="tk-tb todo">{n()}</span>}</Show></span></Show>; }

const LABEL: Record<InspectorTab, string> = { plan: trNoop("Plan & agents"), details: trNoop("Details"), worktrees: trNoop("Worktrees"), tasks: trNoop("Tasks") };
const raw = () => selected() ? followedFor(selected()!.key).value : undefined;
const SHELLS = new Set(["sh", "shell", "bash", "zsh", "pwsh"]);
/** Computers whose account catalog the Inspector already asked for in this run. */
const askedCatalogs = new Set<string>();
function Card(props: { id: string; title: JSX.Element; aside?: JSX.Element; class?: string; classList?: Record<string, boolean>; onContextMenu?: JSX.EventHandler<HTMLElement, MouseEvent>; children: JSX.Element }) {
  const open = () => !inspectorClosed().has(props.id);
  return <section class={`icard ${props.class ?? ""}`} classList={{ closed: !open(), ...props.classList }} onContextMenu={props.onContextMenu}><header class="icard-head">
    <button class="icard-toggle" aria-expanded={open()} aria-controls={`icard-${props.id}`} onClick={() => toggleInspectorCard(props.id)}><span class="icard-title">{props.title}</span><Icon name="chevron-down" size={12} class="icard-chev" /></button>
    <Show when={props.aside}><span class="icard-aside">{props.aside}</span></Show>
  </header><Show when={open()}><div class="icard-body" id={`icard-${props.id}`}>{props.children}</div></Show></section>;
}
function Row(props: { icon: string; label?: string; mono?: boolean; title?: string; tail?: JSX.Element; children: JSX.Element }) {
  return <div class="irow" title={props.title}><span class="irow-ico"><Icon name={props.icon} size={16} /></span><span class="irow-val ell" classList={{ mono: props.mono }}>{props.children}</span>{props.tail}<Show when={props.label}><span class="irow-label">{props.label}</span></Show></div>;
}
function PlanAgents() {
  const s = selected, plan = createMemo(() => sessionPlan(raw())), goal = createMemo(() => sessionGoal(raw()));
  return <div class="insp-body">
    <Card id="plan-main" title={tr("Main agent")} aside={<span class="accent">{s() ? captionText(s()!.caption) : tr("Not reported")}</span>}><Row icon="activity" label={tr("status")}>{s() ? captionText(s()!.caption) : tr("Not reported")}</Row><Row icon="terminal" label={tr("action")} mono title={s()?.summary}>{s()?.summary || tr("Not reported")}</Row></Card>
    <Show when={goal().objective}><Card id="plan-goal" title={tr("Goal")} aside={<span class="muted">{goal().status === "active" ? tr("Pursuing goal") : goal().status}</span>}><p class="icard-text">{goal().objective}</p><Show when={goal().completion}><div class="hint">{tr("Reported by the agent. Completion: %1", goal().completion!)}</div></Show></Card></Show>
    <Show when={plan().length}><Card id="plan-tasks" title={tr("Tasks")} aside={<span class="dim">{tr("%1 of %2 completed (last recorded)", plan().filter((p) => p.status === "completed").length, plan().length)}</span>}><For each={plan()}>{(p) => <Row icon={p.status === "completed" ? "check" : "circle-dot"} label={p.status === "completed" ? tr("Done") : p.status === "in_progress" ? tr("In progress") : tr("To do")}>{p.title}</Row>}</For></Card></Show>
    <Card id="plan-agents" title={tr("Subagents")} aside={<span class="dim">{tr("%1 active / %2 total", s()?.subagents?.filter((c) => c.state === "working").length ?? 0, s()?.subagents?.length ?? 0)}</span>}>
      <For each={s()?.subagents?.slice(0, 100)} fallback={<div class="hint">{tr("No subagent activity has been reported yet.")}</div>}>{(child) => <Row icon="circle" label={child.state === "working" ? tr("Working") : child.state === "ready" ? tr("Ready") : tr("Needs input")} title={child.summary}><button class="link" onClick={() => { setSubagentOpen(child.id); setDetailTab("activity"); }}>{child.name} →</button><span class="dim"> {child.summary}</span></Row>}</For>
    </Card>
  </div>;
}
function Details() {
  const s = () => selected()!;
  const details = () => detailsFor(s(), !computerOffline(s().computer), followedFor(s().key));
  const identity = () => sessionIdentity(s(), details());
  const value = (v: string | undefined) => v || tr("Not reported");
  const rows = createMemo(() => usageRows(raw()));
  const usage = () => sessionUsageView(object(raw()).session_usage);
  const context = () => composer.view()?.context;
  const clear = () => composer.view()?.clear;
  // The listing's recorded binding, else the inspect's (account_id/account_home).
  const bound = createMemo((): BoundRow & { accountId: string; accountHome: string } => {
    const row = s(), v = object(raw());
    return { computer: row.computer, cmd: row.cmd, accountProvider: row.accountProvider, accountReported: row.accountReported, accountId: row.accountId || text(v.account_id), accountHome: row.accountHome || text(v.account_home) };
  }, undefined, { equals: (a, b) => (Object.keys(a) as (keyof BoundRow)[]).every((k) => a[k] === b[k]) });
  const account = createMemo(() => sessionAccount(bound(), accountsPage()));
  // Resolve against the screens hub's catalogs; read a computer's catalog once if none is held.
  createEffect(on(() => s().computer, (computer) => { if (!account().account && !askedCatalogs.has(computer)) { askedCatalogs.add(computer); void openFilterAccounts(); } }));
  const accountText = () => {
    const a = account().account, plan = a?.usage.identity.plan;
    if (a) return plan ? tr("%1 (%2)", a.label || tr("Unnamed account"), plan) : a.label || tr("Unnamed account");
    return bound().accountId || bound().accountHome || (SHELLS.has(s().cmd) ? tr("No account (shell)") : tr("Account not recorded"));
  };
  const home = () => bound().accountHome || account().profile?.home || "";
  const profileId = () => bound().accountId || account().profile?.id || "";
  return <div class="insp-body"><Card id="details-id" title={tr(identity().title)}><Row icon="key-round" mono title={tr(identity().title)} tail={<button class="icon-btn small" aria-label={tr(identity().caption)} title={tr(identity().caption)} disabled={!identity().value} onClick={() => void copyIdentity(snapshotOf(s()), details())}><Icon name="copy" size={13} /></button>}><span class="selectable">{value(identity().value)}</span></Row></Card>
    <Card id="details-session" title={tr("Session")}><Row icon="message-square" label={tr("name")} mono title={s().name}>{s().name}</Row><Row icon="monitor" label={tr("machine")}>{s().machine}</Row><Row icon="folder" label={tr("directory")} mono title={s().cwdPath}>{value(s().cwdPath)}</Row><Row icon="git-branch" label={tr("branch")}>{value(s().branch)}</Row><Row icon="layers" label={tr("project")}>{grouping().headers.find((p) => p.id === groupOf(s().key))?.name ?? tr("Not reported")}</Row><Row icon="clock" label={tr("last event")}>{s().updatedAt ? eventTimeOf(new Date(s().updatedAt! * 1000)) : tr("Not reported")}</Row><div class="hint">{tr("Activity stays on %1. Closing this window does not stop your agent.", s().machine)}</div></Card>
    <Card id="details-account" title={tr("Account")}><Row icon="user-round-cog" label={tr("account")} title={profileId() ? tr("Profile %1", profileId()) : undefined} tail={<Show when={account().tag}><span class="acct-pid tabular" title={tr("Profile %1", profileId())}>{account().tag}</span></Show>}>{accountText()}</Row><Row icon="hard-drive" label={tr("agent home")} mono title={home() || undefined}>{value(homeShort(home()))}</Row><Row icon="sparkles" label={tr("model")}>{value(s().model)} {s().effort}</Row></Card>
    <Card id="details-usage" title={tr("Session usage")}><Show when={rows().length} fallback={<div class="hint">{tr("No native usage reported for this conversation yet.")}</div>}><table class="usage"><tbody><For each={rows()}>{([label, val]) => <tr><td>{label}</td><td class="tabular">{val}</td></tr>}</For></tbody></table><Show when={usage().costNote}><div class="hint">{usage().costNote}</div></Show><div class="hint">{usage().footer}</div></Show></Card>
    <Show when={!subagentOpen()}><div class="insp-foot"><span class="muted" title={context()?.tooltip}>{tr("Context: %1", context()?.text || tr("Not reported"))}</span><span class="grow" /><button class="btn small" title={clear()?.reason || undefined} disabled={!clear()?.enabled} onClick={() => composer.openClear()}>{clear()?.running ? tr("Clearing…") : tr("Clear session…")}</button></div></Show>
  </div>;
}
function Worktrees() {
  const [catalog, setCatalog] = createSignal<Record<string, unknown>>({}), [reading, setReading] = createSignal(false);
  const [picked, setPicked] = createSignal<string>();
  const [catalogError, setCatalogError] = createSignal("");
  const context = createMemo(() => { const s = selected(); return s?.cwdPath ? { computer: s.computer, path: s.cwdPath, key: `${s.computer}\n${s.cwdPath}` } : undefined; });
  let wanted = 0, epoch = 0, answered = false, busy = false, again = false, disposed = false;
  let returnFocus: HTMLElement | undefined, cleanupButton: HTMLButtonElement | undefined;
  const read = async (refresh = false) => {
    if (busy) { again = true; return; }
    const target = context(); if (!target || computerOffline(target.computer)) return;
    busy = true; setReading(true); setCatalogError(""); const ticket = ++wanted;
    try { const value = await bridge.worktreeCatalog(target.computer, target.path, refresh); if (!disposed && ticket === wanted && target.key === context()?.key) { answered = true; setCatalog(value); } }
    catch (error) { if (!disposed && target.key === context()?.key) setCatalogError(String(error)); }
    finally { busy = false; if (!disposed) { setReading(false); if (again) { again = false; void read(); } else if (returnFocus) { const active = document.activeElement, tab = document.querySelector(".inspector .insp-tabs [aria-selected=true]"); if (active === document.body || active === tab) (returnFocus.isConnected ? returnFocus : cleanupButton)?.focus(); returnFocus = undefined; } } }
  };
  createEffect(on(() => context()?.key, () => { ++wanted; const generation = ++epoch; answered = false; setCatalog({}); setCatalogError(""); setPicked(undefined); setMenu(undefined); const target = context(); if (target) void bridge.worktreeSnapshot(target.computer, target.path).then((value) => { if (!disposed && generation === epoch && !answered) setCatalog(value); }).catch(() => {}); void read(); }));
  createEffect(on(worktreeReviewDialog, (dialog, previous) => { if (!dialog && previous) { returnFocus = previous.origin; void read(true); } }));
  onCleanup(() => { disposed = true; ++wanted; setMenu(undefined); });
  const style = () => of(machineCards[""]?.platform, selected()?.computer === "", /Win/.test(navigator.platform) ? "windows" : "posix");
  const trees = createMemo(() => catalogRoots(catalog(), context()?.path ?? ""));
  const current = () => trees().find((t) => text(t.path) === picked()) ?? trees().find((t) => text(t.path) === text(catalog().selected_root)) ?? trees()[0];
  const online = () => !!context() && !computerOffline(context()!.computer);
  createEffect(on(online, (connected, previous) => { if (connected && previous === false) void read(); }));
  const fresh = () => online() && !reading() && !catalogError() && catalog().stale !== true && ["ok", "not_repo"].includes(text(catalog().state));
  const canCleanup = () => fresh() && catalog().state === "ok" && !!text(catalog().common_dir) && trees().some((t) => t.kind === "linked");
  const review = (focus: string, origin?: HTMLElement) => { const target = context(); if (target && canCleanup()) void openWorktreeReview(target.computer, target.path, focus, origin); };
  const menu = (event: MouseEvent & { currentTarget: HTMLElement }, tree: Record<string, unknown>) => {
    event.preventDefault(); const path = text(tree.path), target = context(); setPicked(path);
    const origin = event.currentTarget.querySelector<HTMLElement>(".wt-path") ?? undefined;
    setMenu({ x: event.clientX, y: event.clientY, items: [
      ...(tree.kind === "linked" ? [{ label: tree.available === false ? tr("Forget missing worktree…") : tr("Remove worktree…"), icon: "trash", danger: true, disabled: !canCleanup(), hint: tr("Review this worktree on its computer before anything is removed."), action: () => { if (target?.key === context()?.key) review(path, origin); } }, { separator: true as const }] : []),
      { label: tr("Copy path"), icon: "copy", action: () => { void copy(path); } },
    ] });
  };
  // One catalog pass, retaining at most fifty shown sessions per displayed folder/checkout.
  const sessions = createMemo(() => {
    const roots = new Map<string, Session[]>(), folders = new Map<string, Session[]>();
    for (const tree of trees()) (tree.kind === "folder" ? folders : roots).set(sameKey(text(tree.path), style()), []);
    for (const s of orderedRows()) {
      if (s.computer !== selected()?.computer) continue;
      const root = roots.get(sameKey(s.gitRoot, style())), folder = folders.get(sameKey(s.cwdCanonical, style()));
      if (root && root.length < 50) root.push(s);
      if (folder && folder.length < 50) folder.push(s);
    }
    return (tree: Record<string, unknown>) => (tree.kind === "folder" ? folders : roots).get(sameKey(text(tree.path), style())) ?? [];
  });
  return <div class="insp-body"><div class="insp-h"><b>{tr("Folder and worktrees")}</b><span class="grow" /><button class="icon-btn small" aria-label={tr("Refresh")} title={tr("Refresh")} disabled={reading() || !online()} onClick={() => void read(true)}><Icon name="refresh-cw" size={13} /></button></div>
    <div class="hint" style={{ "white-space": "pre-line" }} classList={{ warn: !online() || !!catalogError() || catalog().partial === true || catalog().stale === true, danger: catalog().state === "folder_unavailable" }}>{catalogStatus(context()?.path ?? "", context()?.computer || machineCards[""]?.host || "this computer", catalog(), online(), reading(), catalogError()).replace(/\.?\nSnapshot:/u, ". Snapshot:")}</div>
    <For each={trees()}>{(tree) => <Card id={`wt-${text(tree.path)}`} class="wt" onContextMenu={(e) => menu(e, tree)} classList={{ on: text(tree.path) === text(current()?.path), gone: tree.available === false }} title={<><b>{tree.kind === "folder" ? tr("Folder") : tree.kind === "main" ? tr("Main checkout") : tree.kind === "bare" ? tr("Bare repository") : tr("Linked worktree")}</b><Show when={text(tree.branch)}><span class="wt-branch"> / {text(tree.branch)}</span></Show><Show when={tree.detached === true}><span class="wt-branch"> / {tr("detached %1", text(tree.head).slice(0, 8))}</span></Show></>} aside={<><Show when={tree.locked === true}><span class="dim">{tr("(locked)")}</span></Show><Show when={tree.prunable === true}><span class="warn">{tr("(prunable)")}</span></Show><Show when={tree.available === false}><span class="danger">{tr("(unavailable)")}</span></Show><Show when={sessions()(tree).length}><span class="dim">{sessions()(tree).length === 1 ? tr("(1 session)") : tr("(%1 sessions)", sessions()(tree).length)}</span></Show></>}>
      <button class="irow wt-path" title={tr("Show the sessions in this folder")} onClick={() => { const host = selected()?.computer; setPicked(text(tree.path)); if (host !== undefined) filterFolder({ computer: host, path: text(tree.path), checkout: tree.kind !== "folder", style: style() }); }}>
        <span class="irow-ico"><Icon name={tree.kind === "folder" ? "folder" : tree.kind === "main" ? "folder-git-2" : tree.kind === "bare" ? "box" : "git-branch"} size={16} stroke={1.75} /></span><span class="irow-val ell mono">{native(text(tree.path), style())}</span><span class="irow-label">{tr("folder")}</span>
      </button>
      <For each={sessions()(tree)}>{(s) => <Row icon="corner-down-right" label={captionText(s.caption)}><button class="link" onClick={() => { setSelectedKey(s.key); setDetailTab("activity"); }}>{s.title}</button></Row>}</For>
    </Card>}</For>
    <div class="insp-foot"><button class="btn small" onClick={() => { setPicked(undefined); clearFolderFilter(); }}>{tr("All sessions")}</button><button ref={cleanupButton} class="btn small" disabled={!canCleanup()} title={tr("Review this repository's worktrees on its computer before anything is removed")} onClick={(e) => review("", e.currentTarget)}>{tr("Clean up…")}</button><span class="grow" /><button class="btn small" disabled={!fresh() || !current() || current()?.available === false || current()?.kind === "bare"} onClick={() => { const s = selected(), path = text(current()?.path); if (s && path && fresh()) void newSessionFromWorktree(groupOf(s.key), s.computer, path); }}><Icon name="plus" size={12} />{tr("New session…")}</button></div>
  </div>;
}
export function Inspector(props: { available: number }) {
  const geometry = () => panelGeometry(props.available, inspectorWidth());
  let panel!: HTMLElement;
  let returnFocus: HTMLElement | undefined;
  const restoreFocus = () => queueMicrotask(() => { if (returnFocus?.isConnected && returnFocus !== document.body) returnFocus.focus(); else document.querySelector<HTMLButtonElement>("[data-cmd='show-side-panel']")?.focus(); });
  createEffect(on(inspectorOpen, (open, previous) => {
    if (open) returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    else if (previous) restoreFocus();
  }));
  const close = () => setInspectorOpen(false);
  createEffect(() => {
    if (!geometry().floating || !inspectorOpen()) return;
    const escape = (e: KeyboardEvent) => { if (e.key === "Escape" && !document.querySelector("dialog[open], [data-shortcut-context='none']")) { e.preventDefault(); close(); } };
    window.addEventListener("keydown", escape); onCleanup(() => window.removeEventListener("keydown", escape));
  });
  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return; const el = e.currentTarget as HTMLElement; el.setPointerCapture(e.pointerId);
    const x = e.clientX, start = geometry().width;
    const move = (v: PointerEvent) => setInspectorWidth(start + x - v.clientX);
    const end = () => { el.removeEventListener("pointermove", move); el.removeEventListener("pointerup", end); el.removeEventListener("pointercancel", end); };
    el.addEventListener("pointermove", move); el.addEventListener("pointerup", end); el.addEventListener("pointercancel", end);
  };
  return <Show when={inspectorOpen()}><aside ref={panel} class="inspector" classList={{ floating: geometry().floating }} style={{ width: `${geometry().width}px` }} aria-label={tr("Side panel")}>
    <div class="insp-resize" tabindex="0" role="separator" aria-orientation="vertical" aria-label={tr("Side panel width")} aria-valuemin={240} aria-valuemax={900} aria-valuenow={inspectorWidth()} onPointerDown={resize} onDblClick={() => setInspectorWidth(400)} onKeyDown={(e) => { if (["ArrowLeft", "ArrowRight", "Home"].includes(e.key)) { e.preventDefault(); setInspectorWidth(e.key === "Home" ? 400 : inspectorWidth() + (e.key === "ArrowLeft" ? 20 : -20)); } }}><i /></div>
    <div class="insp-tabs tabs" role="tablist" aria-label={tr("Side panel tabs")}><For each={INSPECTOR_TABS}>{(tab) => <button class="tab" role="tab" classList={{ on: inspectorTab() === tab }} title={tr(LABEL[tab])} aria-selected={inspectorTab() === tab} tabindex={inspectorTab() === tab ? 0 : -1} onClick={() => setInspectorTab(tab)} onKeyDown={(e) => { const i = INSPECTOR_TABS.indexOf(tab), next = e.key === "ArrowRight" ? (i + 1) % 4 : e.key === "ArrowLeft" ? (i + 3) % 4 : e.key === "Home" ? 0 : e.key === "End" ? 3 : -1; if (next >= 0) { e.preventDefault(); setInspectorTab(INSPECTOR_TABS[next]); panel.querySelectorAll<HTMLButtonElement>(".insp-tabs .tab")[next]?.focus(); } }}><span class="tab-label">{tr(LABEL[tab])}</span>{tab === "tasks" ? <TaskTabBadge /> : null}</button>}</For><span class="grow" /><button class="icon-btn small" aria-label={tr("Collapse side panel")} data-tip={inspectorHint(tr("Collapse side panel"))} data-tip-pos="left" onClick={close}><Icon name="panel-right-close" size={14} /></button></div>
    <div class="insp-scroll" classList={{ tasks: inspectorTab() === "tasks" }}><Switch><Match when={inspectorTab() === "plan"}><PlanAgents /></Match><Match when={inspectorTab() === "details" && selected()}><Details /></Match><Match when={inspectorTab() === "worktrees"}><Worktrees /></Match><Match when={inspectorTab() === "tasks"}><Suspense><TasksPanel /></Suspense></Match></Switch></div>
  </aside></Show>;
}
