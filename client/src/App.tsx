import { showQtUpgradeNotice } from "./store/qtUpgrade";
import { ModelServersScreen } from "./ui/screens/ModelServers";
import { DesktopServiceStatus } from "./ui/components/DesktopServiceStatus";
import { MachinesStatus } from "./ui/components/MachinesStatus";
import { BackgroundTasksChip } from "./ui/screens/BackgroundTasks";
import { CleanupChip } from "./ui/screens/CleanupChip";
import { worktreeReviewDialog } from "./store/worktreeCleanup";
import { cleanupDialog } from "./store/terminationCleanup";
import { taskAssignmentPopover } from "./store/taskAssignment";
import { taskExecutorReview } from "./store/taskExecutors";
import { taskStartReview } from "./store/taskStart";
import { taskSettings } from "./store/taskSettings";
import { taskCancellation } from "./store/taskCancel";
import { Match, Show, Suspense, Switch, createEffect, createSignal, lazy, onCleanup, onMount } from "solid-js";
import { Rail } from "./ui/components/Rail";
import { MenuLayer, Toasts } from "./ui/components/Overlays";
import { ActionDialogs } from "./ui/sessions/ActionDialogs";
import { newSessionFromShortcut, newSessionOpened } from "./ui/sessions/newsession-view";
import { OrganizationDialogs } from "./ui/sessions/OrganizationDialogs";
import { SessionList } from "./ui/sessions/SessionList";
import { SessionDetail } from "./ui/sessions/SessionDetail";
import { CreationPanel } from "./ui/sessions/CreationPanel";
import { selectedCreation } from "./store/creations";
import { applySkin, skin } from "./skin/runtime";
import { contentScale, page, selectedKey, setContentScale, setPage, toast } from "./store/ui";
import { num, setSetting } from "./store/settings";
import { installShortcutRouter, onCommand } from "./store/shortcuts";
import { openGoTo, openShortcutHelp, goToOrigin, shortcutHelpOrigin } from "./store/goTo";
import { rememberRecentSession } from "./ui/sessions/goTo";
import { installWindowCommands } from "./ui/sessions/window-commands";
import { updateCommands, updates } from "./store/updates";
import { openUpdates } from "./store/desk";
import { SessionUsageStrip } from "./ui/components/SessionUsageStrip";
import type { BridgeMode } from "./bridge";
import { installUiLanguage, tr, uiLanguage } from "./i18n";
import { teams } from "./store/teams";
import { lazyView } from "./ui/lazy-view";
import { LazyBoundary } from "./ui/lazy-boundary";
const ParentDialogs = lazyView(() => import("./ui/sessions/ParentDialogs"), (m) => m.ParentDialogs);

// The client shell: the rail, the pages and the status bar.
const WorktreeCleanup = lazy(() => import("./ui/screens/WorktreeCleanup"));
const WorktreeReviewDialog = lazy(() => import("./ui/screens/WorktreeReviewDialog"));

// Accounts keeps its data and refresh cadence in store/accounts; only its
// screen loads on first navigation.
const OverviewScreen = lazy(() => import("./ui/screens/Overview").then((m) => ({ default: m.OverviewScreen })));
const MachinesScreen = lazy(() => import("./ui/screens/Machines").then((m) => ({ default: m.MachinesScreen })));
const AccountsScreen = lazy(() => import("./ui/screens/Accounts").then((m) => ({ default: m.AccountsScreen })));
const ProjectsScreen = lazy(() => import("./ui/screens/Projects").then((m) => ({ default: m.ProjectsScreen })));
// Settings and its sections load the first time Settings opens; their
// state lives in their modules, so it stays once loaded.
const SettingsScreen = lazy(() => import("./ui/screens/Settings").then((m) => ({ default: m.SettingsScreen })));
// New session and its child dialogs load the first time it opens
// (`newSessionOpened`), and stay mounted from then on.
const NewSessionDialog = lazy(() => import("./ui/sessions/NewSessionDialog").then((m) => ({ default: m.NewSessionDialog })));
const TaskAssignmentPopover = lazy(() => import("./ui/screens/TaskAssignmentPopover"));
const TaskExecutorReview = lazy(() => import("./ui/screens/TaskExecutorReview"));
const TaskStartReview = lazy(() => import("./ui/screens/TaskStartReview"));
const TaskSettingsDialog = lazy(() => import("./ui/screens/TaskSettingsDialog"));
const TaskCancelDialog = lazy(() => import("./ui/screens/TaskCancelDialog"));
const GoToDialog = lazy(() => import("./ui/sessions/GoToDialog").then((m) => ({ default: m.GoToDialog })));
const ShortcutHelpDialog = lazy(() => import("./ui/sessions/ShortcutHelpDialog").then((m) => ({ default: m.ShortcutHelpDialog })));
const PerfHost = lazy(() => import("./ui/PerfHost").then((m) => ({ default: m.PerfHost })));
// New team loads the first time it opens; its catalog read happens then.
const NewTeamDialog = lazy(() => import("./ui/teams/NewTeam").then((m) => ({ default: m.NewTeamDialog })));
// Waiting for you loads the first time its rail entry opens.
const WaitingScreen = lazy(() => import("./ui/screens/Waiting").then((m) => ({ default: m.WaitingScreen })));

function LoginItemButton() {
  return (
    <Show when={updates().login_item_deprioritized}>
      <button type="button" class="chip-accent warn-chip" title={tr("macOS limits background priority for this login item")} onClick={openUpdates}>{tr("Fix login item")}</button>
    </Show>
  );
}

function StatusBar(props: { mode: () => BridgeMode | "starting" }) {
  return (
    <footer class="statusbar">
      <MachinesStatus mode={props.mode} />
      <DesktopServiceStatus />
      <Show when={props.mode() === "replay"}>
        <span class="chip-accent" title={tr("Synthetic data from the replay fixture; no hgs is running")}>{tr("Replay")}</span>
      </Show>
      <Show when={!updates().quiet && updates().available}>
        <button type="button" class="chip-accent status-update" title={tr("Zerus %1 is available. Open Updates.", updates().version)}
          aria-label={tr("Zerus %1 is available. Open Updates.", updates().version)} onClick={openUpdates}>{tr("New version")}</button>
      </Show>
      <Show when={!updates().quiet && updates().restart_required}>
        <button type="button" class="chip-accent status-restart" title={tr("Zerus was updated on disk. Drafts are kept; sessions keep running.")}
          aria-label={tr("Zerus was updated on disk. Drafts are kept; sessions keep running.")}
          onClick={() => { void updateCommands.restart().catch((error) => toast("Updates", String(error), "error", true)); }}>{tr("Restart to finish update")}</button>
      </Show>
      <LoginItemButton />
      <BackgroundTasksChip />
      <CleanupChip />
      <span class="grow" />
      <SessionUsageStrip />
    </footer>
  );
}

/** Keyboard zoom changes Settings > Appearance > Content scale (Qt's syncContentScale). */
function zoom(scale: number) {
  setContentScale(scale);
  void setSetting("workspace/contentScale", scale);
}

export function App(props: { mode: () => BridgeMode | "starting" }) {
  createEffect(() => { if (props.mode() === "tauri") void showQtUpgradeNotice(); });
  installUiLanguage();
  createEffect(() => { document.documentElement.lang = uiLanguage(); });
  createEffect(() => applySkin(skin()));
  createEffect(() => setContentScale(num("workspace/contentScale")));
  createEffect(() => rememberRecentSession(selectedKey()));
  // Window commands through the shortcut router (store/shortcuts.ts), with the
  // chords of Settings > Keyboard.
  const zoomBy = (step: number) => {
    zoom(Math.max(0.75, Math.min(2, +(contentScale() + step).toFixed(2))));
    toast(tr("Content scale"), `${Math.round(contentScale() * 100)}%`);
  };
  const commands: [string, () => void][] = [
    ["go.overview", () => setPage("dashboard")],
    ["go.sessions", () => setPage("sessions")],
    ["go.projects", () => setPage("projects")],
    ["go.machines", () => setPage("machines")],
    ["go.accounts", () => setPage("accounts")],
    ["go.settings", () => setPage("settings")],
    ["go.anywhere", openGoTo],
    ["help.shortcuts", openShortcutHelp],
    // session.new (Shortcuts.cpp:29, 58): New session from every page.
    ["session.new", newSessionFromShortcut],
    // Teams are a preview: the dialog opens only when the core says they exist.
    ["team.new", () => { if (teams.available() === true) void teams.openNewTeam(); }],
    ["zoom.in", () => zoomBy(0.1)],
    ["zoom.out", () => zoomBy(-0.1)],
    ["zoom.reset", () => zoom(1)],
  ];
  onMount(() => {
    // One read answers whether Teams are on for this computer; the entry points show only then.
    void teams.probe();
    // Discovery and visibility belong to the application: a hidden window
    // pauses every Teams read, whether or not a Team tab was ever opened.
    const onVisibility = () => { void teams.hidden(document.hidden).catch(() => {}); };
    document.addEventListener("visibilitychange", onVisibility);
    onVisibility();
    const off = [installShortcutRouter(), ...commands.map(([id, run]) => onCommand(id, run)), installWindowCommands(),
      () => document.removeEventListener("visibilitychange", onVisibility)];
    onCleanup(() => off.forEach((undo) => undo()));
  });
  return (
    <div class="app" classList={{ "rail-labeled": skin().layout.rail === "labeled" }}>
      <Rail />
      <main class="page">
        <Switch>
          <Match when={page() === "sessions"}>
            <div class="sessions-page" classList={{ "layout-right": skin().layout["sessions-pane"] === "right" }}>
              <SessionList />
              <Show when={selectedCreation() !== undefined} fallback={<SessionDetail />}><CreationPanel /></Show>
            </div>
          </Match>
          <Match when={page() === "dashboard"}><Suspense><OverviewScreen /></Suspense></Match>
          <Match when={page() === "machines"}><MachinesScreen /></Match>
          <Match when={page() === "accounts"}><Suspense><AccountsScreen /></Suspense></Match>
          <Match when={page() === "projects"}><Suspense><ProjectsScreen /></Suspense></Match>
          <Match when={page() === "waiting"}><Suspense><WaitingScreen /></Suspense></Match>
          <Match when={page() === "modelservers"}><ModelServersScreen /></Match>
          <Match when={page() === "settings"}><Suspense><SettingsScreen /></Suspense></Match>
        </Switch>
      </main>
      <StatusBar mode={props.mode} />
      <MenuLayer />
      <Toasts />
      <ActionDialogs />
      <Show when={cleanupDialog()}><Suspense><WorktreeCleanup /></Suspense></Show>
      <Show when={worktreeReviewDialog()}><Suspense><WorktreeReviewDialog /></Suspense></Show>
      <Show when={taskAssignmentPopover()}><Suspense><TaskAssignmentPopover /></Suspense></Show>
      <Show when={taskExecutorReview()}><Suspense><TaskExecutorReview /></Suspense></Show>
      <Show when={taskStartReview()}><Suspense><TaskStartReview /></Suspense></Show>
      <Show when={taskSettings.dialog()}><Suspense><TaskSettingsDialog /></Suspense></Show>
      <Show when={taskCancellation()}><Suspense><TaskCancelDialog /></Suspense></Show>
      <Show when={goToOrigin()}><Suspense><GoToDialog /></Suspense></Show>
      <Show when={shortcutHelpOrigin()}><Suspense><ShortcutHelpDialog /></Suspense></Show>
      <Show when={newSessionOpened()}><Suspense><NewSessionDialog /></Suspense></Show>
      <Show when={teams.newTeamOpen()}><Suspense><NewTeamDialog /></Suspense></Show>
      <OrganizationDialogs />
      <LazyBoundary floating><ParentDialogs /></LazyBoundary>
      <Suspense><PerfHost /></Suspense>
    </div>
  );
}
