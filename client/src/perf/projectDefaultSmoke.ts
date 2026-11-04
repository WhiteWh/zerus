/** Private replay/fake hgs only: the real checkbox and local layout restart. */
import { invoke } from "@tauri-apps/api/core";
import { grouping } from "../store/organization";
import { projectsPage } from "../store/projects";
import { setPage } from "../store/ui";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until(f: () => unknown) { const end = performance.now() + 20_000; while (!f()) { if (performance.now() > end) throw Error("default did not settle"); await sleep(20); } }
export async function projectDefaultSmoke(allowed: boolean, restored: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "project-default-smoke: " + line });
  try {
    setPage("projects"); await until(() => grouping().ready && projectsPage().projects.some((p) => p.id === "p-sample-project") && document.querySelector(".proj-default input"));
    document.querySelector<HTMLButtonElement>('[data-id="p-sample-project"]')!.click();
    const input = () => document.querySelector<HTMLInputElement>(".proj-default input")!;
    if (!restored) {
      if (input().checked) throw Error("fixture unexpectedly starts with selected default");
      input().click();
      await until(() => input().checked && projectsPage().projects.some((p) => p.id === "p-sample-project" && p.is_default));
      await log("CHOSEN"); await sleep(500);
      input().click(); if (!input().checked) throw Error("unchecking removed the one default");
      let refused = false; try { await invoke("set_default_project", { project: "missing-project" }); } catch { refused = true; }
      if (!refused || grouping().default_project !== "p-sample-project") throw Error("missing project changed the default");
    } else {
      await until(() => input().checked && grouping().default_project === "p-sample-project");
      await log("RESTORED"); await sleep(500);
    }
    await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error)); await invoke("exit_app", { code: 1 }); }
}
