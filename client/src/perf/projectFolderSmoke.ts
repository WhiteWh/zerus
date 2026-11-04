/** Synthetic private folders and fake hgs only; the production widgets/writer. */
import { invoke } from "@tauri-apps/api/core";
import * as bridge from "../bridge";
import { projectsPage } from "../store/projects";
import { grouping } from "../store/organization";
import { setPage } from "../store/ui";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until(f: () => unknown) { const end = performance.now() + 15_000; while (!f()) { if (performance.now() > end) throw Error("folder UI did not settle"); await sleep(20); } }
function require(value: unknown, text: string) { if (!value) throw Error(text); }
export async function projectFolderSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "project-folder-smoke: " + line });
  const button = (text: string, scope: ParentNode = document) => [...scope.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === text)!;
  const project = (id = "p-sample-project") => projectsPage().projects.find((p) => p.id === id)!;
  const dialog = () => document.querySelector<HTMLDialogElement>('dialog[aria-labelledby="project-folder-title"]')!;
  const input = (id: string, value: string) => { const el = document.querySelector<HTMLInputElement>(id)!; el.value = value; el.dispatchEvent(new InputEvent("input", { bubbles: true })); };
  const menu = async () => { await until(() => !button("Edit folder…").disabled); document.querySelector('.proj-table tr[aria-selected="true"]')!.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 600, clientY: 320 })); await until(() => !!document.querySelector(".menu")); };
  try {
    setPage("projects"); await until(() => grouping().ready && project()?.folders.length === 2 && button("Edit folder…"));
    document.querySelector<HTMLButtonElement>('[data-id="p-sample-project"]')!.click();
    const original = (await bridge.projectFolders("p-sample-project")).find((f) => f.id === "folder-a")!;
    await until(() => !button("Open folder").disabled); button("Open folder").click();
    const foreign = (await bridge.projectFolders("p-sample-project")).find((f) => f.id === "folder-b")!;
    let refusedOpen = false; try { await bridge.projectFolderOpen("p-sample-project", foreign); } catch { refusedOpen = true; } require(refusedOpen, "foreign physical machine opened a local folder");
    button("Edit folder…").click(); await until(() => dialog()?.open);
    require(document.querySelector<HTMLInputElement>("#pf-name")!.value === "", "empty original label was replaced by its display name");
    input("#pf-name", "Preview only"); button("Cancel", dialog()).click(); await until(() => !dialog());
    require(document.activeElement === button("Edit folder…"), "Cancel did not return focus");
    button("Edit folder…").click(); await until(() => dialog()?.open);
    require(document.querySelector<HTMLInputElement>("#pf-name")!.value === "", "Cancel saved its preview");
    await log("EDIT"); await sleep(350);
    input("#pf-path", original.path.replace(/folder-a$/u, "folder-b"));
    require(button("Save folder", dialog()).disabled, "duplicate was accepted"); await log("DUPLICATE"); await sleep(350);
    input("#pf-path", original.path); input("#pf-name", "Updated source"); button("Save folder", dialog()).click();
    await until(() => !dialog() && project().folders.some((f) => f.id === "folder-a" && f.name === "Updated source"));
    const stale = await invoke<{ outcome: string }>("project_folder_change", { project: "p-sample-project", change: { kind: "remove", original } }); require(stale.outcome === "refused", "stale source was removed");
    const wrong = await invoke<{ outcome: string }>("project_folder_change", { project: "p-sample-project", change: { kind: "add", place: { computer: "", host: "workstation", machine_id: "changed-owner" }, path: original.path, name: "" } }); require(wrong.outcome === "refused", "replaced computer accepted");
    await log("SAVED");
    await menu(); [...document.querySelectorAll<HTMLButtonElement>(".menu button")].filter((b) => b.textContent?.trim() === "aurora")[0].click();
    await until(() => project("p-aurora").folders.length === 1 && !button("Edit folder…").disabled); await log("COPIED");
    await menu(); [...document.querySelectorAll<HTMLButtonElement>(".menu button")].filter((b) => b.textContent?.trim() === "aurora")[1].click();
    await until(() => project().folders.length === 1 && project("p-aurora").folders.length === 1 && !button("Add folder…").disabled); await log("MOVED");
    button("Add folder…").click(); await until(() => dialog()?.open); input("#pf-path", original.path.replace(/folder-a$/u, "folder-c")); input("#pf-name", "New folder"); button("Add folder", dialog()).click();
    await until(() => !dialog() && project().folders.length === 2 && document.querySelector('.proj-table tr[aria-selected="true"]')?.textContent?.includes("New folder")); await log("ADDED"); await sleep(350);
    const other = [...document.querySelectorAll<HTMLTableRowElement>(".proj-table tbody tr")].find((tr) => tr.textContent?.includes("Second folder"))!; other.click(); button("Remove folder").click();
    await until(() => project().folders.length === 1 && project().folders[0].name === "New folder" && !button("Add folder…").disabled);
    await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error)); await invoke("exit_app", { code: 1 }); }
}
