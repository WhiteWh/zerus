/** Private replay/fake-CLI native adapter walk. No production activation. */
import { invoke } from "@tauri-apps/api/core";
import { projectsPage } from "../store/projects";
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
interface View { id: string; busy: boolean; error: boolean; status: string; aliases: string[]; can_join: boolean; can_sync: boolean; preview: { same_swarm: boolean } | null }
export async function swarmSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "swarm-smoke: " + line });
  const require = (value: unknown, reason: string) => { if (!value) throw Error(reason); };
  async function until(test: () => boolean | Promise<boolean>, reason: string) { const deadline = performance.now() + 20_000; while (!await test()) { if (performance.now() > deadline) throw Error(reason); await sleep(30); } }
  try {
    await until(() => projectsPage().loaded && !!document.querySelector(".projects .su-head"), "Projects did not render");
    await log("PROJECTS"); await sleep(500);
    let view = await invoke<View>("project_swarm_open"); const id = view.id;
    const read = async () => { view = await invoke<View>("project_swarm_view", { id }); return view; };
    await until(async () => { await read(); return !view.busy && view.aliases.includes("fixture-peer"); }, "saved alias read did not settle");
    require(!view.preview, "opening offered a Join without Preview"); await log("OPEN");
    view = await invoke<View>("project_swarm_preview", { id, alias: "fixture-peer" });
    await until(async () => { await read(); return !view.busy && !!view.preview; }, "preview did not settle");
    require(view.preview?.same_swarm && view.can_join, "same-catalog preview was not pinned"); await log("PREVIEW");
    view = await invoke<View>("project_swarm_join", { id, mappings: [], keep: false });
    await until(async () => { await read(); return !view.busy && view.status === "Connected to fixture-peer."; }, "Join/source barrier did not settle");
    require(!view.preview && !view.error, "Join retained replay input or showed failure"); await log("JOIN");
    let refused = false; try { await invoke("project_swarm_join", { id, mappings: [], keep: false }); } catch { refused = true; }
    require(refused, "old preview could be reused");
    await invoke("project_swarm_sync", { id });
    await until(async () => { await read(); return !view.busy && view.status.startsWith("Synchronization finished."); }, "Sync/source barrier did not settle");
    const conflicts = await invoke<{ total: number; rows: { id: string }[] }>("project_swarm_conflicts", { id, page: 0 });
    require(conflicts.total === 1, "native conflict page missing");
    const reviewed = await invoke<{ id: string; chosen: number | null; choices: { total: number } }>("project_swarm_conflict_review", { id, query: { action: "open", conflict: conflicts.rows[0].id } });
    require(reviewed.chosen === null && reviewed.choices.total === 2, "review selected a value by itself");
    await invoke("project_swarm_conflict_review", { id, query: { action: "choose", review: reviewed.id, choice: 1 } });
    await log("CONFLICT_REVIEW");
    await invoke("project_swarm_resolve", { id, review: reviewed.id, confirmed: false });
    await until(async () => { await read(); return !view.busy && !view.can_sync; }, "Resolve did not retain its post-call source barrier");
    require(!view.error && view.status === "Saving your choice to the shared catalog…", "Resolve failed instead of waiting for the exact source");
    refused = false; try { await invoke("project_swarm_resolve", { id, review: reviewed.id, confirmed: true }); } catch { refused = true; }
    require(refused, "old conflict review could be reused");
    await log("RESOLVE_HELD");
    await invoke("project_swarm_close", { id });
    await log("PASS main-window native API, saved alias, exact preview, one Join/Sync, native-held choice and exact Resolve, held source barrier and no replay");
    await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error) + (error instanceof Error ? "\n" + error.stack : "")); await invoke("exit_app", { code: 1 }); }
}
