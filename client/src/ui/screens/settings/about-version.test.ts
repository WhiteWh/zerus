import { describe, expect, it } from "vitest";
import { installedAppName, installedVersion, railVersion } from "./about-version";

const commit = "abcdef0123456789abcdef0123456789abcdef01";

describe("the installed version", () => {
  it("reads as UpdateController::installedVersion", () => {
    // A numbered Dev build is its version name.
    expect(installedVersion({ versionName: "0.47.0-dev.1520", devBuildNumber: 1520, sourceCommit: commit, sourceDirty: false })).toBe("0.47.0-dev.1520");
    // A local preview names itself.
    expect(installedVersion({ versionName: "0.47.0-preview.mac.abcdef0", devBuildNumber: 0, sourceCommit: commit, previewLabel: "mac" })).toBe("0.47.0-preview.mac.abcdef0");
    // An unnumbered build names its source, and local changes to it.
    expect(installedVersion({ versionName: "0.46.2", devBuildNumber: 0, sourceCommit: commit, sourceDirty: false })).toBe("0.46.2 (abcdef0)");
    expect(installedVersion({ versionName: "0.46.2", devBuildNumber: 0, sourceCommit: commit, sourceDirty: true })).toBe("0.46.2 (local changes abcdef0)");
    // Without its source, the version alone.
    expect(installedVersion({ versionName: "0.46.2", devBuildNumber: 0, sourceCommit: null, sourceDirty: null })).toBe("0.46.2");
  });
});

describe("a WhiteWh fork build", () => {
  it("names its fork build number in About and in the title bar", () => {
    const fork = { version: "0.49.0", versionName: "0.49.0", devBuildNumber: 0, sourceCommit: commit, sourceDirty: false, forkBuild: 198 };
    expect(installedAppName(fork)).toBe("Zerus fork build 198, 0.49.0 (abcdef0)");
    expect(railVersion(fork)).toBe("Build 198");
    expect(railVersion({ ...fork, forkBuild: 0 })).toBe("0.49.0");
  });
});

describe("the rail's installed build", () => {
  it("a Stable build never says Dev, even with a source commit or local changes", () => {
    for (const sourceDirty of [false, true]) {
      const stable = { version: "0.48.0", versionName: "0.48.0", devBuildNumber: 0, sourceCommit: commit, sourceDirty };
      expect(railVersion(stable)).toBe("0.48.0");
      expect(installedAppName(stable)).not.toContain("Dev");
      expect(installedAppName(stable)).toContain("0.48.0");
    }
    expect(railVersion({ version: "0.48.1" })).toBe("0.48.1");
  });

  it("uses the numbered Dev base, not the previous Stable or frontend package", () => {
    const dev = { version: "0.47.0", versionName: "0.48.0-dev.1520", devBuildNumber: 1520, sourceCommit: commit };
    expect(railVersion(dev)).toBe("0.48.0\nDev");
    expect(installedAppName(dev)).toBe("Zerus 0.48.0-dev.1520");
  });

  it("does not invent a channel for previews, Nightly versions or an unknown build", () => {
    for (const versionName of ["0.48.0-preview.mac.abcdef0", "0.48.0.r123.gabcdef0.n7"]) {
      expect(railVersion({ versionName, devBuildNumber: 0 })).toBe(versionName);
    }
    expect(railVersion()).toBe("");
    expect(installedAppName()).toBe("Zerus");
  });
});
