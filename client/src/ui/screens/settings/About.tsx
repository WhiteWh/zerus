import { DesktopServiceStatus } from "../../components/DesktopServiceStatus";
import { createSignal, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../../../bridge";
import { Icon } from "../../lib/icons";
import { SectionHead } from "./ui";
import { installedVersion, type BuildInfo } from "./about-version";
import { tr, trNoop } from "../../../i18n";

// Settings > About (tray/src/SettingsPage.h:146-155): the installed Zerus
// as UpdateController::installedVersion names it, the hgs CLI, checked
// whenever the section opens, and the repository.

const [zerus, setZerus] = createSignal(`Zerus ${__APP_VERSION__}`);
// The build's identity does not change while it runs: asked for once.
let identified = false;
const [cli, setCli] = createSignal("hgs CLI version unavailable");
// HgsClient::requestVersion: one `hgs --version` at a time.
let checking = false;

/** Qt's refreshCliVersion: "checking" until `hgs --version` answers. */
export function refreshAbout() {
  if (!inTauri()) return;
  if (!identified) {
    identified = true;
    invoke<BuildInfo>("build_info")
      .then((info) => { const version = installedVersion(info); if (version) setZerus(`Zerus ${version}`); })
      .catch(() => { identified = false; });
  }
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
  return (
    <>
      <SectionHead title={trNoop("About")} />
      <p class="selectable">{zerus()}</p>
      <p class="selectable">{cli()}</p>
      <DesktopServiceStatus counts />
      <div>
        <button class="btn" title="https://github.com/ufna/zerus" aria-label={tr("Open Zerus repository on GitHub")}
          onClick={() => { if (inTauri()) void invoke("open_repository").catch(() => {}); }}>
          <Icon name="github" size={16} />ufna/zerus
        </button>
      </div>
    </>
  );
}
