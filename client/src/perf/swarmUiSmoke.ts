/** Private built-app walk; enabled only with replay and an explicitly fake CLI. */
import { invoke } from "@tauri-apps/api/core";
import { projectsPage } from "../store/projects";
import { page } from "../store/ui";
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
const until = async (test: () => unknown) => { for (let i = 0; i < 500; ++i) { if (test()) return; await sleep(30); } throw Error("Swarm UI timed out"); };
const need = (value: unknown, text: string) => { if (!value) throw Error(text); };
export async function swarmUiSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "swarm-ui-smoke: " + line });
  const dialog = () => document.querySelector<HTMLDialogElement>('dialog[aria-labelledby="project-swarm-title"]');
  const button = (text: string, root: ParentNode = dialog() ?? document) => [...root.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === text)!;
  const capture = async (name: string) => { await until(() => !!dialog() && getComputedStyle(dialog()!).opacity === "1"); await log(name); await sleep(500); };
  const choose = (value: string) => { const select = dialog()!.querySelector<HTMLSelectElement>('[aria-label="Computer to connect"]')!; select.value = value; select.dispatchEvent(new Event("change", { bubbles: true })); };
  const preview = () => button("Preview connection").click();
  const status = () => dialog()!.querySelector('[role="status"]')?.textContent ?? "";
  try {
    await until(() => projectsPage().loaded && button("Swarm…", document) && !button("Swarm…", document).disabled);
    await until(() => !!button("Connect project catalogs…", document));
    button("Connect project catalogs…", document).click();
    await until(() => dialog()?.open && !button("Preview connection").disabled);
    need(dialog()!.querySelector<HTMLSelectElement>('[aria-label="Computer to connect"]')?.value === "fixture-uninitialized", "notice did not preselect the unconnected computer");
    await capture("PRESELECTED"); button("Close").click(); await until(() => !dialog());
    const origin = button("Swarm…", document); origin.focus(); origin.click();
    await until(() => dialog()?.open && !button("Sync now").disabled);
    need(dialog()!.querySelectorAll(".sw-table tbody tr").length === 3, "Computers rows missing");
    await capture("COMPUTERS");
    const filter = dialog()!.querySelector<HTMLInputElement>('input[type="search"]')!;
    filter.value = "fixture-peer"; filter.dispatchEvent(new Event("input", { bubbles: true }));
    await until(() => dialog()!.querySelectorAll(".sw-table tbody tr").length === 1); await capture("FILTERED");
    filter.value = ""; filter.dispatchEvent(new Event("input", { bubbles: true }));
    button("Sync now").click(); await until(() => status().startsWith("Synchronization finished.") && !button("Sync now").disabled);
    button("Connect computer").click(); await until(() => !button("Preview connection").disabled);
    choose("fixture-peer"); preview(); await until(() => dialog()!.querySelector('[role="progressbar"]')); await capture("LOADING");
    await until(() => !!dialog()!.querySelector(".sw-map") && !button("Join and merge projects").disabled);
    const choices = [...dialog()!.querySelectorAll<HTMLSelectElement>(".sw-map select")];
    await log("MAP_VALUES " + JSON.stringify({ total: choices.length, inbox: choices.find((s) => s.getAttribute("aria-label") === "Match Inbox")?.value, separate: choices.filter((s) => !s.value).length }));
    need(choices.length === projectsPage().projects.length && choices.every((s) => s.getAttribute("aria-label") === "Match Inbox" ? s.value === "ungrouped" : s.value === ""), "identical Inbox ID must match; distinct IDs must stay separate");
    choices[0].value = "remote-project"; choices[0].dispatchEvent(new Event("change", { bubbles: true }));
    need(dialog()!.querySelector<HTMLInputElement>('.check input')?.checked, "keep-placement default missing"); await capture("MAP");
    choose("fixture-uninitialized"); need(button("Connect this member").disabled, "changing computer reused a Preview"); preview();
    await until(() => status().startsWith("Open Projects in Zerus on fixture-uninitialized"));
    need(button("Connect this member").disabled, "uninitialized catalog admitted Join"); await capture("UNINITIALIZED");
    choose("fixture-failed"); preview(); await until(() => status().includes("Fixture peer is unavailable"));
    need(button("Connect this member").disabled, "failed Preview admitted Join"); await capture("FAILED");
    choose("fixture-peer"); preview(); await until(() => !!dialog()!.querySelector(".sw-msg")?.textContent?.includes("already shares") && !button("Connect this member").disabled); await capture("SAME");
    button("Connect this member").click(); await until(() => status() === "Connected to fixture-peer.");
    need(button("Connect this member").disabled, "completed Preview could be replayed"); await capture("CONNECTED");
    await log("ESCAPE_SWARM"); await until(() => !dialog()); await until(() => document.activeElement === origin);
    origin.click(); await until(() => dialog()?.open && !button("Sync now").disabled);
    button("Manage connections…").click(); await until(() => !dialog() && page() === "machines");
    await log("PASS mockup Computers/filter/Sync, loading/mapping/separate defaults, unavailable/uninitialized refusal, exact same-catalog Join once, Escape/focus and Manage connections; no creation/removal");
    await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error) + (error instanceof Error ? "\n" + error.stack : "")); await invoke("exit_app", { code: 1 }); }
}
