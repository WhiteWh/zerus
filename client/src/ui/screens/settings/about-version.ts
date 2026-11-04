// UpdateController::installedVersion: what About says this build is, from
// the same facts `--build-info` prints (the app's `build_info`).

import { tr } from "../../../i18n";

export interface BuildInfo {
  version?: string;
  versionName?: string;
  devBuildNumber?: number;
  sourceCommit?: string | null;
  sourceDirty?: boolean | null;
  previewLabel?: string;
}

/** A numbered or preview build, or one without its source, is its version
 *  name; any other names its source commit, and local changes to it. */
export function installedVersion(info: BuildInfo): string {
  const version = info.versionName ?? info.version ?? "";
  const commit = info.sourceCommit ?? "";
  if ((info.devBuildNumber ?? 0) > 0 || info.previewLabel !== undefined || !commit) return version;
  const source = commit.slice(0, 7);
  return info.sourceDirty ? tr("%1 (local changes %2)", version, source) : `${version} (${source})`;
}
