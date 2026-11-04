import { DesktopServiceStatus } from "./ui/components/DesktopServiceStatus";
import { desktopService } from "./store/desktopService";
import { CleanupChip } from "./ui/screens/CleanupChip";
import { worktreeReviewDialog } from "./store/worktreeCleanup";
import { cleanupDialog } from "./store/terminationCleanup";
import { taskAssignmentPopover } from "./store/taskAssignment";
import { taskExecutorReview } from "./store/taskExecutors";
import { taskStartReview } from "./store/taskStart";
import { taskSettings } from "./store/taskSettings";
import { taskCancellation } from "./store/taskCancel";
import { For, Match, Show, Suspense, Switch, createEffect, createMemo, createSignal, lazy, onCleanup, onMount } from "solid-js";
import { Rail } from "./ui/components/Rail";
import { MenuLayer, Toasts } from "./ui/components/Overlays";
import { ActionDialogs } from "./ui/sessions/ActionDialogs";
import { newSessionFromShortcut, newSessionOpened } from "./ui/sessions/newsession-view";
import { OrganizationDialogs } from "./ui/sessions/OrganizationDialogs";
import { SessionList } from "./ui/sessions/SessionList";
import { SessionDetail } from "./ui/sessions/SessionDetail";
import { CreationPanel } from "./ui/sessions/CreationPanel";
import { selectedCreation } from "./store/creations";
import { OverviewScreen } from "./ui/screens/Overview";
import { ProjectsScreen } from "./ui/screens/Projects";
import { applySkin, skin } from "./skin/runtime";
import { catalog } from "./store/catalog";
import { contentScale, page, selectedKey, setContentScale, setPage, toast } from "./store/ui";
import { num, setSetting } from "./store/settings";
import { installShortcutRouter, onCommand } from "./store/shortcuts";
import { openGoTo, openShortcutHelp, goToOrigin, shortcutHelpOrigin } from "./store/goTo";
import { rememberRecentSession } from "./ui/sessions/goTo";
import { updateCommands, updates } from "./store/updates";
import { openUpdates } from "./store/desk";
import { SessionUsageStrip } from "./ui/components/SessionUsageStrip";
import { fleetRefresh, type BridgeMode } from "./bridge";
import { installUiLanguage, tr, uiLanguage } from "./i18n";

// The client shell: the rail, the pages and the status bar.
const WorktreeCleanup = lazy(() => import("./ui/screens/WorktreeCleanup"));
const WorktreeReviewDialog = lazy(() => import("./ui/screens/WorktreeReviewDialog"));

// Accounts keeps its data and refresh cadence in store/accounts; only its
// screen loads on first navigation.
const MachinesScreen = lazy(() => import("./ui/screens/Machines").then((m) => ({ default: m.MachinesScreen })));
const AccountsScreen = lazy(() => import("./ui/screens/Accounts").then((m) => ({ default: m.AccountsScreen })));
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

const STATE_TEXT: Record<string, string> = {
  probing: "checking",
  unsupported: "no event stream (update hgs)",
  polled: "polled every 60 s (update hgs for live updates)",
  reconnecting: "reconnecting",
  ended: "stream ended",
};

function LoginItemButton() {
  return (
    <Show when={updates().login_item_deprioritized}>
      <button type="button" class="chip-accent warn-chip" title={tr("macOS limits background priority for this login item")} onClick={openUpdates}>{tr("Fix login item")}</button>
    </Show>
  );
}

function StatusBar(props: { mode: () => BridgeMode | "starting" }) {
  const computers = createMemo(() => Object.values(catalog.computers));
  const connected = (status: string) => status === "live" || status === "polled";
  const live = createMemo(() => computers().filter((c) => connected(c.status)).length);
  const problems = createMemo(() => computers().filter((c) => !connected(c.status)));
  const label = (id: string) => id || tr("this computer");
  const connectionError = createMemo(() => problems().some((c) => c.status !== "probing"));
  const summary = createMemo(() => tr("%1 of %2 machines connected", live(), computers().length) + (problems().length ? tr(", %1 offline", problems().map((c) => label(c.id)).join(", ")) : ""));
  return (
    <footer class="statusbar">
      <Show when={computers().length} fallback={<span>{props.mode() === "starting" ? tr("Starting…") : tr("Connecting…")}</span>}>
        {connectionError() ? (
          <>
            <span class="danger">
              <For each={problems()}>{(c, i) => <>{i() ? "; " : ""}{label(c.id)}: {tr(STATE_TEXT[c.status] ?? c.status)}{c.status === "reconnecting" && c.attempt ? ` (attempt ${c.attempt})` : ""}</>}</For>
            </span>
            <button type="button" class="btn small ghost" onClick={() => { void fleetRefresh().catch((error) => toast("Connection", String(error), "error", true)); }}>{tr("Retry now")}</button>
          </>
        ) : (
          <span class="conn-ok"><span class="state-dot ready" hidden={problems().length > 0} />{summary()}</span>
        )}
      </Show>
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
    ["zoom.in", () => zoomBy(0.1)],
    ["zoom.out", () => zoomBy(-0.1)],
    ["zoom.reset", () => zoom(1)],
  ];
  onMount(() => {
    const off = [installShortcutRouter(), ...commands.map(([id, run]) => onCommand(id, run))];
    onCleanup(() => off.forEach((undo) => undo()));
  });
  return (
    <div class="app" classList={{ "rail-labeled": skin().layout.rail === "labeled" }}>
      <Rail />
      <main class="page" classList={{ "with-service-status": desktopService.enabled() }}>
        <DesktopServiceStatus />
        <Switch>
          <Match when={page() === "sessions"}>
            <div class="sessions-page" classList={{ "layout-right": skin().layout["sessions-pane"] === "right" }}>
              <SessionList />
              <Show when={selectedCreation() !== undefined} fallback={<SessionDetail />}><CreationPanel /></Show>
            </div>
          </Match>
          <Match when={page() === "dashboard"}><OverviewScreen /></Match>
          <Match when={page() === "machines"}><MachinesScreen /></Match>
          <Match when={page() === "accounts"}><Suspense><AccountsScreen /></Suspense></Match>
          <Match when={page() === "projects"}><ProjectsScreen /></Match>
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
      <OrganizationDialogs />
      <Suspense><PerfHost /></Suspense>
    </div>
  );
}
