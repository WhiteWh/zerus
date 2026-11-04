import { invoke } from "@tauri-apps/api/core";
import { projectsPage } from "../store/projects";
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
const until = async (test: () => unknown) => {for (let i = 0; i < 300; i++) {if (test()) return; await sleep(30);} throw Error("clone UI timed out");};
const need = (value: unknown, text: string) => {if (!value) throw Error(text);};
const log = async (text: string) => {await invoke("perf_log", {line: "project-clone-smoke: " + text});};
export async function projectCloneSmoke(allowed: boolean, restored: boolean) {
  if (!allowed) return;
  await log("BEGIN");
  const dialog = () => document.querySelector<HTMLDialogElement>('dialog[aria-labelledby="project-clone-title"]');
  const button = (text: string, root: ParentNode = document) => [...root.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === text)!;
  const input = (id: string, text: string) => {const e = dialog()!.querySelector<HTMLInputElement>(id)!; e.value = text; e.dispatchEvent(new Event("input", {bubbles: true}));};
  const open = async () => {button("Clone repository…").click(); await until(() => dialog()?.open); await log("CHOOSER"); button("Continue", dialog()!).click(); await until(() => dialog()?.querySelector("#cl-url"));};
  try {
    await until(() => projectsPage().projects.length && button("Clone repository…") && !button("Clone repository…").disabled);
    await open();
    if (!restored) {
      input("#cl-url", "https://example.org/team/repo.git"); input("#cl-parent", "/srv/example/checkouts"); await until(() => (dialog()!.querySelector<HTMLInputElement>("#cl-name"))?.value === "repo");
      await log("FORM"); await sleep(500); button("Clone repository", dialog()!).click();
      await until(() => button("Check result", dialog()!) && !button("Check result", dialog()!).disabled); await log("UNKNOWN"); await sleep(500);
      need((dialog()!.querySelector<HTMLInputElement>("#cl-url"))?.disabled, "unknown clone unlocked"); need(button("Clone repository", dialog()!).disabled, "unknown clone could repeat");
      button("Cancel", dialog()!).click(); await until(() => !dialog()); await open();
      await until(() => button("Check result", dialog()!)); await log("REOPENED"); await invoke("exit_app", {code: 0}); return;
    }
    await until(() => button("Check result", dialog()!)); await log("RESTORED"); button("Check result", dialog()!).click();
    await until(() => button("Use checkout", dialog()!) && !button("Use checkout", dialog()!).disabled); await log("READY"); await sleep(500); button("Use checkout", dialog()!).click();
    await until(() => !dialog() && projectsPage().projects.some((p) => p.folders.some((f) => f.path === "/srv/example/checkouts/repo"))); await log("ADDED");
    await open(); input("#cl-url", "https://example.org/team/another.git"); input("#cl-parent", "/srv/example/checkouts"); await until(() => (dialog()!.querySelector<HTMLInputElement>("#cl-name"))?.value === "another");
    const project = dialog()!.querySelector<HTMLSelectElement>("#cl-proj")!; project.value = "__new__"; project.dispatchEvent(new Event("change", {bubbles: true})); input("#cl-newp", "Fresh clone project");
    button("Clone repository", dialog()!).click(); await until(() => button("Use checkout", dialog()!) && !button("Use checkout", dialog()!).disabled); button("Use checkout", dialog()!).click();
    await until(() => !dialog() && projectsPage().projects.some((p) => p.name === "Fresh clone project" && p.folders.some((f) => f.path === "/srv/example/checkouts/another"))); await log("NEW_PROJECT"); await sleep(500);
    await invoke("exit_app", {code: 0});
  } catch (error) {await log("FAIL " + String(error)); await invoke("exit_app", {code: 1});}
}
