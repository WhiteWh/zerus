import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";

export interface BuildInfo {
  version?: string;
  versionName?: string;
  devBuildNumber?: number;
  sourceCommit?: string | null;
  sourceDirty?: boolean | null;
  previewLabel?: string;
  /** WhiteWh fork: the fork release build this binary embeds (ZERUS_FORK_BUILD). */
  forkBuild?: number;
}

/** One immutable native identity for the rail and About, never an update preference. */
export function createBuildInfo(read: () => Promise<BuildInfo>, available = () => true) {
  const [info, setInfo] = createSignal<BuildInfo>();
  let pending: Promise<void> | undefined;
  function refresh(): Promise<void> {
    if (!available() || info()) return Promise.resolve();
    if (!pending) {
      pending = read().then((value) => { setInfo(value); })
        .catch(() => { /* Keep identity unknown; a later open may retry. */ })
        .finally(() => { pending = undefined; });
    }
    return pending;
  }
  return { info, refresh };
}

const app = createBuildInfo(() => invoke<BuildInfo>("build_info"),
  () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window);
export const appBuildInfo = app.info;
export const refreshAppBuildInfo = app.refresh;
