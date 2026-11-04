import { createSignal } from "solid-js";
import type * as bridge from "../../bridge";
import type { ComputerId, Folder, FolderOut, MachineMeta, PathStyle } from "../../types";
import { absolute, native, of } from "./path-forms";
import { tr } from "../../i18n";

// The Projects entries' state for a folder (ProjectsDialog.cpp 515-532,
// 552-560): why it cannot start a session and its path as shown. The
// Projects screen renders these at once, so they live in the start-up chunk
// without the New session store, which loads on the first open; the store's
// openFolderSession checks the same state before it opens the dialog.

/** A folder a Projects entry starts from: the screen's `Folder` or the organization's `FolderOut`. */
export type LaunchFolder = Pick<Folder | FolderOut, "key" | "machine" | "path" | "machine_id">;

/** .66.2.31 admits a read model only; native Windows folder actions await PC qualification. */
export const localWindowsFolder = (folder: LaunchFolder, local?: MachineMeta): boolean =>
  local?.platform === "windows" && (!!local.machine_id && folder.machine_id === local.machine_id || !!local.host && folder.machine === local.host);

export interface LaunchEnvironment {
  /** This computer's host name, "" before it reports one. */
  localHost: string;
  online(computer: string): boolean;
  /** A folder's computer style by its machine name. */
  style(machine: string): PathStyle;
}

/**
 * Why a project folder cannot start a session (`folderLaunchProblem`), ""
 * when it can. `localExists` is whether this computer's folder exists
 * (`local_folder_exists`); it is asked only for a folder here. Qt's empty
 * id is no folder selected: every listed folder has a key.
 */
export function folderLaunchProblem(folder: LaunchFolder, env: LaunchEnvironment, localExists: boolean | undefined): string {
  if (!folder.key) return tr("Select a folder for the new session.");
  if (!folder.machine || !env.localHost) return tr("Waiting for this computer's identity.");
  const local = folder.machine === env.localHost;
  if (!env.online(local ? "" : folder.machine)) return tr("Reconnect %1 to start a session in this folder.", folder.machine);
  if (!absolute(folder.path, env.style(folder.machine)) || (local && localExists === false)) return tr("This folder is unavailable on this computer.");
  return "";
}

/** What the entries' state reads; the store's dependencies include them. */
export interface FolderLaunchDeps {
  bridge: Pick<typeof bridge, "newSessionContext" | "localFolderExists">;
  /** Machine cards by computer ("" is this computer). */
  cards(): Record<ComputerId, MachineMeta>;
  /** The computer's card is ok. */
  online(computer: ComputerId): boolean;
}

/** The entries' state over its dependencies; the app's one is newsession-view.ts's, which its store shares. */
export function createFolderLaunch(deps: FolderLaunchDeps) {
  // The platform this client was built for: this computer's folder style before it reports one.
  const [buildStyle, setBuildStyle] = createSignal<PathStyle>();
  let styleAsked = false;

  /** The build style, asked once when this computer has not reported its platform. */
  async function ensureBuildStyle() {
    if (deps.cards()[""]?.platform || buildStyle() !== undefined || styleAsked) return;
    styleAsked = true;
    try { setBuildStyle((await deps.bridge.newSessionContext()).build_style); } catch { setBuildStyle("posix"); }
  }

  /**
   * The Projects entries' state for a folder (ProjectsDialog.cpp 515-532,
   * 552-560): why it cannot start a session, "" when it can; its path in its
   * computer's form; whether it is this computer's. `exists` is whether this
   * computer's folder exists, left out while unread. Reactive; reads nothing.
   */
  function folderLaunch(folder: LaunchFolder, exists?: boolean): { problem: string; shown: string; local: boolean } {
    const local = deps.cards()[""];
    const localHost = local?.host ?? "";
    const style = (machine: string) => of(local?.platform, !!machine && machine === localHost, buildStyle() ?? "posix");
    const env = { localHost, online: deps.online, style };
    return { problem: localWindowsFolder(folder, local) ? tr("Folder management is unavailable for this computer.") : folderLaunchProblem(folder, env, exists), shown: native(folder.path, style(folder.machine)), local: !!localHost && folder.machine === localHost };
  }

  /**
   * Whether this computer's folder exists (`local_folder_exists`, a read);
   * undefined for another computer's. It first asks the build style once, so
   * `folderLaunch` follows this computer's style before its card reports one.
   */
  async function folderExists(folder: LaunchFolder): Promise<boolean | undefined> {
    await ensureBuildStyle();
    if (localWindowsFolder(folder, deps.cards()[""])) return false;
    if (!folderLaunch(folder).local) return undefined;
    try { return await deps.bridge.localFolderExists(folder.path); } catch { return false; }
  }

  return { folderLaunch, folderExists, ensureBuildStyle, setBuildStyle };
}

export type FolderLaunchState = ReturnType<typeof createFolderLaunch>;
