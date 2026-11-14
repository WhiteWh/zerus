// UpdateController::installedVersion: what About says this build is, from
// the same facts `--build-info` prints (the app's `build_info`).

import { tr } from "../../../i18n";
import type { BuildInfo } from "../../../store/buildInfo";

/** A numbered or preview build, or one without its source, is its version
 *  name; any other names its source commit, and local changes to it. */
export function installedVersion(info: BuildInfo): string {
  const version = info.versionName ?? info.version ?? "";
  const commit = info.sourceCommit ?? "";
  // WhiteWh fork (owner, 2026-11-14): a fork build names its own build number,
  // the one its releases and updater use, before the upstream version.
  const fork = info.forkBuild ?? 0;
  if (fork > 0) {
    const source = commit ? ` (${commit.slice(0, 7)})` : "";
    return tr("fork build %1, %2", fork, version) + source;
  }
  if ((info.devBuildNumber ?? 0) > 0 || info.previewLabel !== undefined || !commit) return version;
  const source = commit.slice(0, 7);
  return info.sourceDirty ? tr("%1 (local changes %2)", version, source) : `${version} (${source})`;
}

export function installedAppName(info?: BuildInfo): string {
  const version = info ? installedVersion(info) : "";
  return version ? `Zerus ${version}` : "Zerus";
}

/** Keep full Stable versions; only a numbered native Dev identity gets Dev.
 * Build info currently has no packaged Nightly marker (zerus-dsk6q). */
export function railVersion(info?: BuildInfo): string {
  const version = info?.versionName ?? info?.version ?? "";
  const fork = info?.forkBuild ?? 0;
  if (fork > 0) return tr("Build %1", fork);
  return version && (info?.devBuildNumber ?? 0) > 0
    ? `${version.replace(/-dev\.\d+$/, "")}\nDev` : version;
}
