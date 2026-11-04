import { For, Match, Show, Suspense, Switch, createEffect, createMemo, createRoot, createResource, createSignal, lazy, on, onCleanup, untrack } from "solid-js";
import { tr } from "../../i18n";
import * as bridge from "../../bridge";
import { terminal } from "../../store/terminal";
import { TerminalTab } from "./Terminal";
import { NativeUiTab } from "./NativeUi";
import { nativeBinding, nativeEntry, nativeRoute } from "./native-entry";
import { nativeUi } from "../../store/native-ui";
import { pageHidden } from "../../store/visibility";
import { ProcessesTab } from "./Processes";
import { drops } from "../../store/drops";
import { processes, processesTab } from "../../store/processes";
import { flag } from "../../store/settings";
import type { TerminalStatus } from "../../types";
import { Icon } from "../lib/icons";
import { ActivityView, columnStyle } from "./Activity";
import { AgentBadge, MachineBadge, captionText, statusOf, folderOf } from "./SessionCard";
import { detailTab, page, selected, setDetailTab, setMenu, subagentOpen, type MenuEntry } from "../../store/ui";
import { activity, subagentActivity } from "../../store/session";
import { activityReadOnly, type ActivitySource } from "../../store/activitySource";
import { archiveActivity } from "../../store/archiveActivity";
import { computerOffline } from "../../store/catalog";
import { copyIdentity, detailsFor, followedFor } from "../../store/actions";
import { headerClearSlot, sessionIdentity } from "./actions-ui";
import { actionEntries, copyEntries, freshActionEntry } from "./ActionDialogs";
import { snapshotOf } from "./actions-ui";
import { markEntry } from "./mark-entries";
import { followViewedReplies } from "./viewed-reply";
import { Composer } from "./Composer";
import { ComposerDialogs } from "./ComposerDialogs";
import { QuestionCard } from "./QuestionCard";
import { QueuePanel } from "./QueuePanel";
import { onCommand } from "../../store/shortcuts";
import { composer } from "../../store/composer";
import { createSubagentIdentity } from "../../store/subagentIdentity";
import { selectedTask } from "../../store/taskNavigation";
import { Inspector } from "./Inspector";
import { followProjectTasks } from "../../store/taskContext";
import { inspectorOpen, setInspectorOpen, openInspector, inspectorHint } from "../../store/inspector";
import { sessionGoal } from "./inspector-data";

import { folderEntries, folderRequest } from "../../store/sessionFolders";

const TaskView = lazy(() => import("../screens/TaskView"));

// The session header and its Activity with the composer under it, after
// design/webui SessionDetail. Session actions keep the mockup placement and
// use the existing guarded handlers.

function headerCaption(): string {
  const s = selected();
  if (!s) return "";
  return statusOf(s) === "offline" ? tr("Offline") : captionText(s.caption);
}

function EmptyDetail() {
  return (
    <div class="detail-empty">
      <h2>{tr("A little more focus.")}</h2>
      <p>{tr("Select a session to see what your agent is doing.")}<br />{tr("New sessions appear here automatically.")}</p>
    </div>
  );
}

function Header() {
  const s = () => selected()!;
  const tone = () => (statusOf(s()) === "attention" ? "attention" : statusOf(s()));
  // Details come from the followed session's Activity state value; the store
  // inspects only once that subscription answered without one.
  const details = createMemo(() => detailsFor(s(), !computerOffline(s().computer), followedFor(s().key)));
  const childDetails = createSubagentIdentity(bridge.inspectSubagentIdentity);
  createEffect(() => {
    const id = subagentOpen();
    const row = s(), d = details();
    childDetails.follow(id ? { agentId: id, archiveId: row.archiveId ?? null,
      target: { computer: row.computer, name: row.name, run_id: d?.run_id || row.runId || "", conversation_id: d?.conversation_id || row.conversationId || "" } } : undefined);
  });
  onCleanup(() => childDetails.follow(undefined));
  const child = () => childDetails.child() ?? (subagentOpen() ? { id: subagentOpen()! } : undefined);
  const identity = () => sessionIdentity(s(), details(), child());
  const folderScope = createMemo(() => JSON.stringify(folderRequest(s(), details(), child())));
  const [folderStatus] = createResource(folderScope, async (scope) => {
    const request = JSON.parse(scope) as ReturnType<typeof folderRequest>;
    return { scope, status: await bridge.sessionFolderStatus(request.target.computer, request.directory).catch(() => undefined) };
  });
  const folders = () => folderEntries(s(), folderRequest(s(), details(), child()), folderStatus()?.scope === folderScope() ? folderStatus()?.status : undefined);

  const copyId = () => { void copyIdentity(snapshotOf(s()), details(), child()); };
  const act = createMemo(() => actionEntries(s(), details()));
  const copies = createMemo(() => copyEntries(s(), details()));
  const mark = createMemo(() => markEntry(s()));
  const slot = () => headerClearSlot(!subagentOpen(), s().state);
  const fresh = createMemo(() => freshActionEntry(s(), true));
  // Clear session goes through the composer's clear_context (Qt's clearContext): its dialog shows the identity it sends back.
  const clear = () => composer.view()?.clear;
  const clearEntry = (): MenuEntry => ({
    label: tr("Clear session…"), icon: "clear", disabled: !clear()?.enabled, hint: clear()?.reason || undefined, action: () => composer.openClear(),
  });
  const actions = (): MenuEntry[] => [
    folders().folder,
    folders().shell,
    { label: identity().caption, icon: "copy", disabled: !identity().value, action: copyId },
    { separator: true },
    act().rename,
    act().fork,
    { separator: true },
    copies().command,
    ...(nativeEntry(s()) ? [nativeEntry(s())!] : []),
    act().change,
    ...(mark() ? [mark()!] : []),
    { separator: true },
    ...(slot() === "fresh" && fresh() ? [fresh()!] : slot() === "clear" ? [clearEntry()] : []),
    ...(act().archive ? [act().archive!] : []),
    act().end,
  ];
  return (
    <header class="detail-head">
      <div class="dh-row1">
        <h1 class="ell" title={s().name}>{s().project} / {s().title}<Show when={subagentOpen()}> / {s().subagents?.find((k) => k.id === subagentOpen())?.name}</Show></h1>
        <Show when={s().computer !== "" && statusOf(s()) === "working" && !activity.loaded()}><span class="busy-arc" title={tr("Waiting for the remote read")} /></Show>
        <span class="grow" />
        <span class={`hbadge t-${tone()}`}>{headerCaption()}</span>
        <button class="icon-btn" data-tip={folders().folder.hint} aria-label={tr("Open session folder in file manager")} disabled={folders().folder.disabled} onClick={() => folders().folder.action?.()}><Icon name="folder-open" size={16} /></button>
        <button class="icon-btn" data-tip={tr("Open shell in session folder")} aria-label={tr("Open shell in session folder")} disabled={folders().shell.disabled} onClick={() => folders().shell.action?.()}><Icon name="square-terminal" size={16} /></button>
        <button class="icon-btn" data-tip={identity().caption} aria-label={identity().caption} disabled={!identity().value} onClick={copyId}><Icon name="copy" size={15} /></button>
        <Show when={slot() === "clear"}>
          <button class="icon-btn" data-tip={clear()?.reason || tr("Clear session")}
            aria-label={clear()?.running ? tr("Clearing…") : tr("Clear session")} disabled={!clear()?.enabled} onClick={() => composer.openClear()}><Icon name="clear" size={15} /></button>
        </Show>
        <Show when={!subagentOpen() && (s().status === "ended" || s().state === "archived")}><button class="btn small primary" title={act().change.hint} disabled={act().change.disabled} onClick={() => act().change.action()}><Icon name="play" size={12} />{tr("Restore")}</button></Show>
        <button class="icon-btn" data-tip={tr("Session actions")} onClick={(e) => setMenu({ x: e.clientX - 200, y: e.clientY + 12, items: actions() })}><Icon name="ellipsis" size={16} /></button>
      </div>
      <div class="dh-row2">
        <AgentBadge agent={s().agent} />
        <MachineBadge alias={s().machine} computer={s().computer} />
        {/* The branch is a label beside the agent and machine badges, with the branch glyph. */}
        <Show when={s().branch}>
          <span class="dh-branch" title={tr("Git branch %1", s().branch!)}><Icon name="git-branch" size={12} /><span class="ell">{s().branch}</span></span>
        </Show>
        <Show when={s().cwd}>
          <span class="dh-chip ell" title={s().cwd}><Icon name="folder" size={13} />{folderOf(s())}</span>
        </Show>
        <Show when={s().worktree}><span class="dh-chip ell" title={tr("Linked worktree %1", s().worktree!)}><Icon name="folder-git-2" size={13} />{tr("worktree %1", s().worktree!)}</span></Show>
        <span class="grow" />
        <span class="dh-model ell">{s().model} {s().effort ?? ""}</span>
      </div>
    </header>
  );
}

type Tab = { id: string; label: string };
const TABS: readonly Tab[] = [{ id: "activity", label: "Activity" }, { id: "terminal", label: "Terminal" }];

/** The selected child's bounded page, an archive's recorded page, or the live parent follow. */
export const activitySourceFor = (key: string): ActivitySource => (subagentOpen() ? subagentActivity : activityReadOnly(key) ? archiveActivity : activity);

/** The tabs of a row: an archived row has no Terminal (Qt hides only that one; terminal_status answers unsupported). */
export function detailTabs(key: string): readonly Tab[];
export function detailTabs<T extends Tab>(key: string, tabs: readonly T[]): readonly T[];
export function detailTabs(key: string, tabs: readonly Tab[] = TABS): readonly Tab[] {
  return activityReadOnly(key) ? tabs.filter((t) => t.id !== "terminal") : tabs;
}

/** The selected session's terminal status (terminal_status), refreshed when its inputs change. */
const [terminalTexts, setTerminalTexts] = createSignal<TerminalStatus | undefined>();
createRoot(() => {
  createEffect(() => {
    const s = selected(), binding = nativeBinding(s), online = !!s && s.state !== "archived" && !computerOffline(s.computer);
    untrack(() => nativeUi.select(binding, online));
  });
  createEffect(() => {
    const shown = page() === "sessions" && detailTab() === "native" && !subagentOpen() && !pageHidden();
    untrack(() => nativeUi.opened(shown));
  });
  // The inputs of terminal_status as one string: a row change that leaves them alone calls nothing.
  const inputs = createMemo(() => {
    const s = selected();
    if (!s) return undefined;
    const followed = followedFor(s.key);
    const details = detailsFor(s, !computerOffline(s.computer), followed);
    return JSON.stringify([s.key, s.name, s.runId, s.cmd, s.state, s.terminalSupported, s.terminalReported, s.terminalAttachSupported,
      s.terminalAttachReported, !computerOffline(s.computer), followed.value ?? details ?? null]);
  });
  // Another selection: input stops reaching the previous session before anything is awaited.
  createEffect(on(() => selected()?.key, () => { terminal.leaveSession(); }, { defer: true }));
  createEffect(on(inputs, (input) => {
    const s = selected();
    if (!input || !s) { setTerminalTexts(undefined); void terminal.setTarget(undefined, undefined); return; }
    const followed = followedFor(s.key);
    void bridge.terminalStatus({
      facts: { cmd: s.cmd, terminal_supported: s.terminalSupported, terminal_reported: s.terminalReported, terminal_attach_supported: s.terminalAttachSupported,
        terminal_attach_reported: s.terminalAttachReported, terminal_reason: "", terminal_attach_reason: "" },
      details: followed.value ?? detailsFor(s, !computerOffline(s.computer), followed) ?? {},
      state: s.state, online: !computerOffline(s.computer), restored_pending: false, name: s.name, machine: s.machine,
    }).then((status) => {
      if (selected()?.key !== s.key) return;
      setTerminalTexts(status);
      void terminal.setTarget({ computer: s.computer, name: s.name, run_id: s.runId ?? "", machine: s.machine }, status);
    });
  }));
  // Leaving the Sessions page disconnects, and coming back does not connect (SessionsWindow.cpp:1637, QStackedWidget::currentChanged).
  createEffect(on(page, (p) => { if (p !== "sessions") void terminal.leavePage(); }, { defer: true }));
});

const tabs = () => {
  const count = processes.view()?.count ?? 0;
  const all = [
    { id: "activity" as const, label: tr("Activity"), show: true },
    { id: "terminal" as const, label: tr("Terminal"), show: !!terminalTexts()?.supported && !subagentOpen() },
    { id: "native" as const, label: tr("Native UI"), show: selected()?.agent === "deepseek" && !subagentOpen() },
    processesTab(flag("processes/enabled"), count),
    { id: "taskview" as const, label: tr("Task View"), show: detailTab() === "taskview" && !!selectedTask() },
  ];
  const s = selected();
  return s ? detailTabs(s.key, all) : all;
};

// A tab that disappears returns to Activity (SessionsWindow.cpp:4019-4020).
createRoot(() => createEffect(() => { if (!tabs().find((t) => t.id === detailTab())?.show) setDetailTab("activity"); }));

export function SessionDetail() {
  followProjectTasks();
  // A reply shown in Activity or Terminal for a second is read (Qt's checkViewedReply).
  followViewedReplies();
  const [available, setAvailable] = createSignal(900);
  const measure = (el: HTMLDivElement) => {
    const observer = new ResizeObserver(() => setAvailable(el.clientWidth)); observer.observe(el);
    onCleanup(() => observer.disconnect());
  };
  const goal = createMemo(() => sessionGoal(selected() ? followedFor(selected()!.key).value : undefined));
  const signIn = createMemo(() => {
    const s = selected();
    return s?.agent === "deepseek" && s.state === "running" && !computerOffline(s.computer)
      && detailsFor(s, true, followedFor(s.key))?.auth_required;
  });
  const toggle = onCommand("panel.toggle", () => { if (page() === "sessions" && selected()) setInspectorOpen(!inspectorOpen()); });
  onCleanup(toggle);
  createEffect(() => {
    if (page() !== "sessions" || !selected()) return;
    // Window scope on every detail tab (SessionsWindowShortcuts.cpp:202,242).
    const stop = onCommand("queue.sendNow", () => { void composer.sendNow(); });
    onCleanup(stop);
  });
  return (
    <section class="detail" aria-label={tr("Session")}>
      {/* Qt's drop overlay (SessionFileDrop): what a drop on this panel does. */}
      <Show when={drops.hint()}>{(text) => <div class="drop-overlay" aria-hidden="true"><span>{text()}</span></div>}</Show>
      <Show when={selected()}><Header /></Show>
        <div class="work" ref={measure}>
        <Show when={selected()} fallback={<div class="work-main"><EmptyDetail /></div>}>
          <div class="work-main">
            <div class="detail-tabs tabs" role="tablist">
              <For each={tabs().filter((t) => t.show)}>{(t) => (
                <button class="tab" role="tab" classList={{ on: detailTab() === t.id }} aria-selected={detailTab() === t.id} onClick={() => { if (t.id === "terminal") terminal.opened(); setDetailTab(t.id); }}>{t.label}</button>
              )}</For>
              <span class="grow" />
              <Show when={!inspectorOpen()}><button class="icon-btn small" data-cmd="show-side-panel" aria-label={tr("Show side panel")} data-tip={inspectorHint(tr("Show tasks, plan, agents, details and worktrees"))} data-tip-pos="left" onClick={() => setInspectorOpen(true)}><Icon name="panel-right" size={15} /></button></Show>
            </div>
            <div class="tab-body">
              <Switch>
                <Match when={detailTab() === "activity"}>
                  <div class="activity-wrap" classList={{ "has-goal": !!goal().objective }} style={columnStyle()}>
                    <Show when={goal().objective}><button class="goal-pill" onClick={() => openInspector("plan")}><Icon name="circle-dot" size={13} /><b>{tr("Pursuing goal")}</b><span class="ell">{goal().objective}</span><span class="grow" /><span>{tr("Details")}</span><Icon name="chevron-right" size={12} /></button></Show>
                    <ActivityView source={activitySourceFor(selected()!.key)} />
                    {/* Child Activity stays read-only; its own composer opens only when the inspection supports send. */}
                    <Show when={!subagentOpen()}>
                      <Show when={signIn()}><div class="cmp-col"><div class="qcard">
                        <p class="qcard-body">{tr("Sign in to DeepSeek to start using this session. Complete sign-in in the official DeepSeek UI, then return here.")}</p>
                        <div class="qcard-actions"><button class="btn small" onClick={() => nativeRoute(selected())()}>{tr("Sign in to DeepSeek…")}</button></div>
                      </div></div></Show>
                      <QueuePanel /><QuestionCard />
                    </Show>
                    <div class="cmp-col"><Composer /></div>
                  </div>
                </Match>
                <Match when={detailTab() === "taskview"}><Suspense><TaskView /></Suspense></Match>
                <Match when={detailTab() === "terminal"}><TerminalTab /></Match>
                <Match when={detailTab() === "native"}><NativeUiTab /></Match>
                <Match when={detailTab() === "processes"}><ProcessesTab /></Match>
              </Switch>
            </div>
          </div>
        </Show>
        <Show when={selected()}><Inspector available={available()} /></Show>
        </div>
        {/* The composer's questions and Clear session, also from the header on another tab; the field takes the focus back. */}
        <ComposerDialogs onClosed={() => { if (document.activeElement === document.body) document.querySelector<HTMLTextAreaElement>(".cmp-editor")?.focus(); }} />
    </section>
  );
}
