import { createRoot } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import * as bridge from "../../bridge";
import { machineCards } from "../../store/machines";
import type { Folder, MachineMeta, Project } from "../../types";
import { createProjectFolders } from "./project-folders";
import { createFolderLaunch } from "../sessions/folder-launch";

vi.mock("../../bridge", () => ({ projectFolders: vi.fn(), projectFolderOpen: vi.fn() }));
vi.mock("../../store/organization", () => ({ grouping: () => ({ ready: true }), changeProjectFolder: vi.fn() }));
vi.mock("../../store/machines", () => ({ machineCards: {} }));
vi.mock("../../store/notices", () => ({ bus: { notify: vi.fn() } }));
const node = "10000000-0000-4000-8000-000000000001";
const local = { host: "workstation", platform: "windows", machine_id: node } as MachineMeta;
const folder: Folder = { id: "drive", key: "drive", machine: "old-local-name", computer: "Old name", machine_id: node, path: "C:\\Source", name: "Source" };
const project = { id: "alpha", accessible: true, folders: [folder] } as Project;

describe("the Windows Projects read projection", () => {
  it("keeps local metadata edit, removal, transfer and open fenced without any IPC", async () => {
    machineCards[""] = local;
    await createRoot(async (dispose) => {
      const edits = createProjectFolders(() => [project], vi.fn());
      expect(edits.enabled("alpha", folder)).toBe(false);
      await edits.open("alpha", folder, {} as HTMLElement);
      await edits.remove("alpha", folder);
      await edits.transfer("alpha", folder, "other", true);
      await edits.openFolder("alpha", folder);
      expect(edits.prompt()).toBeUndefined();
      expect(bridge.projectFolders).not.toHaveBeenCalled();
      expect(bridge.projectFolderOpen).not.toHaveBeenCalled();
      dispose();
    });
  });
  it("does not probe or launch a projected local drive/UNC, even under its original alias", async () => {
    const localFolderExists = vi.fn(async () => true);
    const launch = createFolderLaunch({ cards: () => ({ "": local }), online: () => true,
      bridge: { localFolderExists, newSessionContext: vi.fn() } });
    for (const path of ["C:\\Source", "\\\\server\\share\\Source"]) {
      const row = { ...folder, path };
      expect(launch.folderLaunch(row).problem).toBe("Folder management is unavailable for this computer.");
      expect(await launch.folderExists(row)).toBe(false);
    }
    expect(localFolderExists).not.toHaveBeenCalled();
    expect(launch.folderLaunch({ ...folder, machine: "peer", machine_id: "other", path: "//server/share" }).problem).toBe("");
  });
});
