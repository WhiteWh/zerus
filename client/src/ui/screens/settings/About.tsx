import { DesktopServiceStatus } from "../../components/DesktopServiceStatus";
import { Show, createSignal, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../../../bridge";
import { Icon } from "../../lib/icons";
import { SectionHead } from "./ui";
import { ServiceModeReview } from "./ServiceModeReview";
import { ServiceRefresh } from "./ServiceRefresh";
import { ServiceFirstChoice } from "./ServiceFirstChoice";
import LoginEntrySetting from "./LoginEntrySetting";
import { installedAppName } from "./about-version";
import { appBuildInfo, refreshAppBuildInfo } from "../../../store/buildInfo";
import { tr, trNoop } from "../../../i18n";
import { lazyView } from "../../lazy-view";
import { LazyBoundary } from "../../lazy-boundary";
const MigrationReportDialog = lazyView(() => import("../../components/MigrationReportDialog"), (m) => m.MigrationReportDialog);

// Settings > About (tray/src/SettingsPage.h:146-155): the installed Zerus
// as UpdateController::installedVersion names it, the hgs CLI, checked
// whenever the section opens, and the repository.

const zerus = () => installedAppName(appBuildInfo());
// WhiteWh fork: a fork build opens the fork's repository (settings.rs).
const repository = () => ((appBuildInfo()?.forkBuild ?? 0) > 0 ? "WhiteWh/zerus" : "ufna/zerus");
const [cli, setCli] = createSignal("hgs CLI version unavailable");
// HgsClient::requestVersion: one `hgs --version` at a time.
let checking = false;

/** Qt's refreshCliVersion: "checking" until `hgs --version` answers. */
export function refreshAbout() {
  if (!inTauri()) return;
  void refreshAppBuildInfo();
  setCli("hgs CLI: checking…");
  if (checking) return;
  checking = true;
  invoke<string | null>("hgs_version")
    .then((version) => setCli(version ? `hgs CLI ${version}` : "hgs CLI version unavailable"))
    .catch(() => setCli("hgs CLI version unavailable"))
    .finally(() => { checking = false; });
}

export function AboutSection() {
  onMount(refreshAbout);
  // The same read-only Migration report the service status offers, in every
  // mode: without the background service it is otherwise unreachable.
  const [reportOrigin, setReportOrigin] = createSignal<HTMLElement>();
  return (
    <>
      <SectionHead title={trNoop("About")} />
      <p class="selectable">{zerus()}</p>
      <p class="selectable">{cli()}</p>
      <LoginEntrySetting />
      <DesktopServiceStatus counts />
      <ServiceModeReview />
      <ServiceFirstChoice />
      <ServiceRefresh />
      <div>
        <button type="button" class="btn" onClick={(event) => setReportOrigin(event.currentTarget)}>
          <Icon name="file-text" size={16} />{tr("Migration report…")}
        </button>
      </div>
      <div>
        <button class="btn" title={`https://github.com/${repository()}`} aria-label={tr("Open Zerus repository on GitHub")}
          onClick={() => { if (inTauri()) void invoke("open_repository").catch(() => {}); }}>
          <Icon name="github" size={16} />{repository()}
        </button>
      </div>
      <Show when={reportOrigin()}>{(origin) => <LazyBoundary floating><MigrationReportDialog origin={origin()} close={() => setReportOrigin(undefined)} /></LazyBoundary>}</Show>
    </>
  );
}
