import { For, Show, createEffect, createMemo, createSignal, on, onCleanup } from "solid-js";
import { Icon } from "../lib/icons";
import * as bridge from "../../bridge";
import type { Folder } from "../../types";
import type { Session } from "../sessions/types";
import { WorktreePicker, catalogRoots } from "../sessions/worktree-dialogs";
import { WorktreeReadLane } from "./worktree-read-lane";
import { WorktreeReminder, cleanupReminderView } from "./worktree-reminder";
import { machineCards } from "../../store/machines";
import { computerOffline, orderedRows } from "../../store/catalog";
import { openWorktreeReview, worktreeReviewDialog } from "../../store/worktreeCleanup";
import { copy } from "../../store/actions";
import { clearFolderFilter, filterFolder } from "../../store/folderFilter";
import { setMenu, setPage, setSelectedKey, setDetailTab } from "../../store/ui";
import { newSessionFromWorktree } from "../sessions/newsession-view";
import { localWindowsFolder } from "../sessions/folder-launch";
import { native, of, sameKey } from "../sessions/paths";
import { text } from "./tasks-ui";
import "../styles/project-worktrees.css";
import { tr } from "../../i18n";
import { captionText } from "../sessions/SessionCard";

/** The mockup's WorktreePanel, using the existing picker reads/status and the
 * same guarded review and session actions as the Inspector's Worktrees tab. */
export default function ProjectWorktrees(props: { folder?: Folder; project: string }) {
  const lane = new WorktreeReadLane();
  let gone = false;
  let returnFocus: HTMLElement | undefined, cleanupButton: HTMLButtonElement | undefined, refreshButton: HTMLButtonElement | undefined;
  const [reader, setReader] = createSignal<WorktreePicker>(); const [version, setVersion] = createSignal(0);
  const [reminderCount, setReminderCount] = createSignal<number>();
  const [visible, setVisible] = createSignal(!document.hidden);
  const reminder = new WorktreeReminder(bridge.worktreeCleanupReminder, setReminderCount);
  const visibility = () => setVisible(!document.hidden);
  document.addEventListener("visibilitychange", visibility);
  const context = createMemo(() => {
    const folder = props.folder; if (!folder?.path || !folder.machine || !machineCards[""]?.host) return;
    if (localWindowsFolder(folder, machineCards[""])) return;
    const computer = folder.machine === machineCards[""].host ? "" : folder.machine;
    const card = machineCards[computer];
    if (folder.machine_id && folder.machine_id !== card?.machine_id) return;
    return {computer, node: card?.machine_id ?? "", machine: folder.machine, path: folder.path, key: `${computer}\n${card?.machine_id ?? ""}\n${folder.key}\n${folder.path}`};
  });
  const online = () => !!context() && !computerOffline(context()!.computer);
  const style = () => of(machineCards[""]?.platform, context()?.computer === "", /Win/.test(navigator.platform) ? "windows" : "posix");
  const listed = createMemo(() => orderedRows().filter((s) => s.computer === context()?.computer));
  const v = createMemo(() => {version(); return reader()?.view(online(), listed(), style());});
  const data = createMemo(() => {version(); return reader()?.catalog() ?? {};});
  const trees = createMemo(() => catalogRoots(data(), context()?.path ?? ""));
  const current = () => trees().find((r) => text(r.path) === v()?.selected);
  createEffect(on(() => context()?.key, () => {
    reader()?.dispose(); setMenu(undefined); const target = context();
    if (!target) {setReader(undefined); return;}
    const panel = new WorktreePicker({snapshot: () => bridge.worktreeSnapshot(target.computer, target.path), catalog: (refresh) => lane.read(() => target.key === context()?.key && online() ? bridge.worktreeCatalog(target.computer, target.path, refresh) : Promise.reject(tr("Folder changed.")))}, target.path, target.machine);
    panel.onChange = () => setVersion((n) => n + 1); setReader(panel); panel.show(online());
  }));
  createEffect(on(online, (yes, old) => {if (yes && old === false) reader()?.show(true);}));
  createEffect(on(worktreeReviewDialog, (next, old) => {if (!next && old) {returnFocus = old.origin; reminder.refresh(); reader()?.refresh(online());}}));
  createEffect(() => {
    const reading = v()?.reading;
    if (!returnFocus || reading) return;
    const origin = returnFocus; returnFocus = undefined;
    queueMicrotask(() => {if (gone || (document.activeElement !== document.body && document.activeElement !== origin)) return; if (origin.isConnected && !(origin instanceof HTMLButtonElement && origin.disabled)) origin.focus(); else if (cleanupButton && !cleanupButton.disabled) cleanupButton.focus(); else if (refreshButton && !refreshButton.disabled) refreshButton.focus();});
  });
  onCleanup(() => {gone = true; reminder.dispose(); document.removeEventListener("visibilitychange", visibility); reader()?.dispose(); lane.close(); setMenu(undefined);});
  const cleanup = createMemo(() => cleanupReminderView(data().state, online() && !!v()?.fresh, text(data().common_dir), trees().some((r) => r.kind === "linked"), reminderCount()));
  const canCleanup = () => cleanup().enabled;
  createEffect(() => {
    const target = context();
    reminder.show(target && target.node && canCleanup() ? {key: target.key, computer: target.computer, machine: target.node, path: target.path, common: text(data().common_dir)} : undefined, visible());
  });
  const review = (focus: string, origin?: HTMLElement) => {const target = context(); if (target && canCleanup()) void openWorktreeReview(target.computer, target.path, focus, origin);};
  const menu = (event: MouseEvent, row: Record<string, unknown>) => {
    event.preventDefault(); const target = context(), path = text(row.path); reader()?.select(path); const origin = event.currentTarget as HTMLElement;
    setMenu({x: event.clientX, y: event.clientY, items: [
      ...(row.kind === "linked" ? [{label: row.available === false ? tr("Forget missing worktree…") : tr("Remove worktree…"), icon: "trash", danger: true, disabled: !canCleanup(), action: () => {if (target?.key === context()?.key) review(path, origin);}}, {separator: true as const}] : []),
      {label: tr("Copy path"), icon: "copy", action: () => {void copy(path);}},
    ]});
  };
  // One pass and at most fifty shown children per displayed checkout, as in
  // the Inspector. The picker retains exact total counts independently.
  const related = createMemo(() => {
    const roots = new Map<string, Session[]>(), folders = new Map<string, Session[]>();
    for (const row of trees()) (row.kind === "folder" ? folders : roots).set(sameKey(text(row.path), style()), []);
    for (const session of listed()) {
      const root = roots.get(sameKey(session.gitRoot, style())), folder = folders.get(sameKey(session.cwdCanonical, style()));
      if (root && root.length < 50) root.push(session); if (folder && folder.length < 50) folder.push(session);
    }
    return (row: Record<string, unknown>) => (row.kind === "folder" ? folders : roots).get(sameKey(text(row.path), style())) ?? [];
  });
  const count = (path: string) => v()?.rows.find((r) => r.path === path)?.sessions ?? 0;
  const selectSessions = (row: Record<string, unknown>) => {const target = context(); if (target) filterFolder({computer: target.computer, path: text(row.path), checkout: row.kind !== "folder", style: style()});};
  return <section class="wtp" aria-label={tr("Folder and worktrees")}>
    <header class="wtp-head"><b>{tr("Folder and worktrees")}</b><button ref={refreshButton} class="icon-btn small" title={tr("Refresh")} aria-label={tr("Refresh")} disabled={!v()?.refreshEnabled} onClick={() => reader()?.refresh(online())}><Icon name="refresh-cw" size={13} /></button><span class="grow" /><button class="btn small" disabled={!context()} onClick={clearFolderFilter}>{tr("All sessions")}</button><button ref={cleanupButton} class="btn small" disabled={!canCleanup()} title={cleanup().title} onClick={(e) => review("", e.currentTarget)}>{cleanup().label}</button><button class="btn small" disabled={!v()?.useEnabled} onClick={() => {const target = context(), row = current(); if (target && row && v()?.useEnabled) void newSessionFromWorktree(props.project, target.computer, text(row.path));}}><Icon name="plus" size={12} />{tr("New session…")}</button></header>
    <div class="wtp-status" classList={{warn: !!context() && (!online() || (!v()?.reading && !v()?.fresh) || data().partial === true || data().stale === true), danger: data().state === "folder_unavailable"}}>{v()?.status.replace(/\.?\nSnapshot:/u, ". Snapshot:") ?? (props.folder ? localWindowsFolder(props.folder, machineCards[""]) ? tr("Folder management is unavailable for this computer.") : machineCards[""]?.host ? tr("The selected computer changed. Select it again.") : tr("Waiting for this computer's identity.") : tr("Select a folder."))}</div>
    <Show when={context()}><ul class="wtp-tree" role="tree" aria-label={tr("Checkouts")}><For each={trees()}>{(row) => <li class="wtp-root" classList={{on: text(row.path) === v()?.selected, gone: row.available === false}} role="treeitem" aria-selected={text(row.path) === v()?.selected}>
      <button class="wtp-row" onClick={() => reader()?.select(text(row.path))} onDblClick={() => selectSessions(row)} onContextMenu={(e) => menu(e, row)} title={tr("Click to select; double-click to show its sessions")}>
        <Icon name={row.kind === "main" ? "folder-git-2" : row.kind === "bare" ? "box" : row.kind === "folder" ? "folder" : "git-branch"} size={14} class="wtp-ico" />
        <span class="wtp-title ell"><b>{row.kind === "main" ? tr("Main checkout") : row.kind === "bare" ? tr("Bare repository") : row.kind === "folder" ? tr("Folder") : tr("Linked worktree")}</b><Show when={text(row.branch)}> / {text(row.branch)}</Show><Show when={row.detached === true}> / detached {text(row.head).slice(0, 8)}</Show></span>
        <Show when={row.locked === true}><span class="wtp-tag">{tr("(locked)")}</span></Show><Show when={row.prunable === true}><span class="wtp-tag warn">{tr("(prunable)")}</span></Show><Show when={row.available === false}><span class="wtp-tag danger">{tr("(unavailable)")}</span></Show><Show when={count(text(row.path))}>{(n) => <span class="wtp-tag">({n()} {n() === 1 ? "session" : "sessions"})</span>}</Show>
      </button><div class="wtp-path mono ell" title={text(row.path)}>{native(text(row.path), style())}</div>
      <Show when={related()(row).length}><ul class="wtp-kids" role="group"><For each={related()(row)}>{(session) => <li><button class="wtp-kid" onClick={() => {setSelectedKey(session.key); setDetailTab("activity"); setPage("sessions");}}><Icon name="corner-down-right" size={12} class="dim" /><span>{session.title}</span><span class="dim">({online() ? session.caption : tr("last recorded: %1", captionText(session.caption))})</span></button></li>}</For></ul></Show>
    </li>}</For></ul></Show>
  </section>;
}
