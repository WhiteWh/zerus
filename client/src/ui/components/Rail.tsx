import { For, Show, createEffect, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { ZerusLogo } from "./ZerusLogo";
import { page, setPage, counts, setMenu, logoMotion, type Page } from "../../store/ui";
import { markAllEntry } from "../sessions/mark-entries";
import { expandAllCollapsedEntry } from "../sessions/background-menu";
import { windowLayer, setWindowLayer } from "../../store/windowLayer";
import { skin } from "../../skin/runtime";
import { newSessionFromRail } from "../sessions/newsession-view";
import { attentionBadgeText } from "./rail-badge";
import { shortcutTooltip } from "../../store/shortcuts";
import { openAbout } from "../../store/desk";
import { trNoop, tr } from "../../i18n";
import { appBuildInfo, refreshAppBuildInfo } from "../../store/buildInfo";
import { installedAppName, railVersion } from "../screens/settings/about-version";
import { teams } from "../../store/teams";
import { modelServers } from "../../store/modelServers";

// The side screen switcher stays on the side (owner decision 2026-10-27).
// Order follows SessionsWindow: brand -> Overview, New session, Sessions,
// Projects, Machines, Accounts; window pin, Settings and the version at the bottom.
// Glyphs and the 24px icon size follow tray/src/SessionsWindow.cpp:328-351.
// Sessions is the one live screen of this phase; the others show where they will be.

const ITEMS: { id: Page; icon: string; label: string; tip: string; command: string }[] = [
  { id: "sessions", icon: "sessions", label: "Sessions", tip: "Sessions", command: "go.sessions" },
  { id: "projects", icon: "projects", label: "Projects", tip: "Projects", command: "go.projects" },
  { id: "machines", icon: "machines", label: "Machines", tip: trNoop("Machines and connection status"), command: "go.machines" },
  { id: "accounts", icon: "accounts", label: "Accounts", tip: trNoop("Agent accounts"), command: "go.accounts" },
  // Model servers, shown while this computer has one.
  { id: "modelservers", icon: "hard-drive", label: trNoop("Model servers"), tip: trNoop("Model servers"), command: "go.modelservers" },
];

export function Rail() {
  const waitingTip = () => {
    const count = teams.waitingCount();
    return count > 0 ? tr("Waiting for you — %1", count) : tr("Waiting for you");
  };
  onMount(() => { void refreshAppBuildInfo(); void modelServers.refreshList(); });
  createEffect(() => { if (!modelServers.available() && modelServers.listed() && page() === "modelservers") setPage("sessions"); });
  const shown = () => ITEMS.filter((item) => item.id !== "modelservers" || modelServers.available());
  const labeled = () => skin().layout.rail === "labeled";
  return (
    <nav class="rail" classList={{ labeled: labeled() }} aria-label={tr("Screens")}>
      <button class="rail-brand" classList={{ on: page() === "dashboard" }} data-tip={shortcutTooltip(tr("Home — %1", installedAppName(appBuildInfo())), "go.overview")} data-tip-pos="right" onClick={() => setPage("dashboard")}>
        <ZerusLogo attention={counts().attention > 0} motion={logoMotion()} size={28} />
        <Show when={labeled()}><span>{tr("Overview")}</span></Show>
      </button>
      <button class="rail-new" data-tip={shortcutTooltip(tr("New session"), "session.new")} data-tip-pos="right" onClick={newSessionFromRail}>
        <Icon name="add" size={24} />
        <Show when={labeled()}><span>{tr("New")}</span></Show>
      </button>
      <Show when={teams.available() === true}>
        <button class="rail-btn" classList={{ on: page() === "waiting" }} data-tip={labeled() ? undefined : waitingTip()} data-tip-pos="right"
          aria-label={waitingTip()} onClick={() => setPage("waiting")}>
          <Icon name="message-square" size={24} />
          <Show when={teams.waitingCount() > 0}><span class="rail-badge" aria-hidden="true">{teams.waitingCount() > 99 ? "99+" : teams.waitingCount()}</span></Show>
          <Show when={labeled()}><span>{tr("Waiting")}</span></Show>
        </button>
      </Show>
      <Show when={teams.available() === true}>
        <button class="rail-btn" data-tip={labeled() ? undefined : tr("New team")} data-tip-pos="right" aria-label={tr("New team")} onClick={() => void teams.openNewTeam()}>
          <Icon name="users" size={24} />
          <Show when={labeled()}><span>{tr("New team")}</span></Show>
        </button>
      </Show>
      <For each={shown()}>
        {(item) => (
          <button class="rail-btn" classList={{ on: page() === item.id }} aria-current={page() === item.id ? "page" : undefined}
            aria-label={item.id === "sessions" && counts().attention > 0 ? tr("Sessions — %1 need attention", counts().attention) : tr(item.label)}
            // A labeled rail shows its names, but the capped badge keeps its exact count on hover, as Qt always did (zerus-nfmh1).
            data-tip={item.id === "sessions" && counts().attention > 0 ? shortcutTooltip(tr("Sessions — %1 need attention", counts().attention), item.command)
              : labeled() ? undefined : shortcutTooltip(tr(item.tip), item.command)} data-tip-pos="right" onClick={() => setPage(item.id)}
            onContextMenu={(e) => { if (item.id !== "sessions") return; e.preventDefault(); setMenu({ x: e.clientX, y: e.clientY, items: [markAllEntry(), expandAllCollapsedEntry()] }); }}>
            <Icon name={item.icon} size={24} />
            <Show when={item.id === "sessions" && counts().attention > 0}>
              <span class="rail-badge" aria-hidden="true">{attentionBadgeText(counts().attention)}</span>
            </Show>
            <Show when={labeled()}><span>{tr(item.label)}</span></Show>
          </button>
        )}
      </For>
      <div class="grow" />
      <button class="rail-btn pin" classList={{ pinned: windowLayer().on }} aria-pressed={windowLayer().on} aria-label={tr("Keep Zerus above other windows")} disabled={!windowLayer().supported}
        data-tip={!windowLayer().supported ? windowLayer().hint : windowLayer().on ? tr("Unpin Zerus") : tr("Keep Zerus above other windows")} data-tip-pos="right"
        onClick={() => void setWindowLayer(!windowLayer().on)}>
        <Icon name={windowLayer().on ? "pinned" : "unpinned"} size={24} />
      </button>
      <button class="rail-btn" classList={{ on: page() === "settings" }} data-tip={labeled() ? undefined : shortcutTooltip(tr("Settings"), "go.settings")} data-tip-pos="right" onClick={() => setPage("settings")}>
        <Icon name="settings" size={24} />
        <Show when={labeled()}><span>{tr("Settings")}</span></Show>
      </button>
      <button class="rail-version" data-tip={tr("%1\nOpen About", installedAppName(appBuildInfo()))} data-tip-pos="right" onClick={openAbout}>{railVersion(appBuildInfo()) || tr("About")}</button>
    </nav>
  );
}
