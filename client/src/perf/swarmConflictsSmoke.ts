/** Private replay plus fake hgs only; ordinary and overflow mockup fixtures. */
import { invoke } from "@tauri-apps/api/core";
import { projectsPage } from "../store/projects";
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
export async function swarmConflictsSmoke(allowed: boolean, large: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "swarm-conflicts-smoke: " + line });
  const need = (yes: unknown, reason: string) => { if (!yes) throw Error(reason); };
  const until = async (test: () => unknown) => { for (let i = 0; i < 500; ++i) { if (test()) return; await sleep(30); } throw Error("Conflicts UI timed out"); };
  const dialog = () => document.querySelector<HTMLDialogElement>('dialog[aria-labelledby="project-swarm-title"]');
  const confirmation = () => document.querySelector<HTMLDialogElement>('dialog[aria-labelledby="swarm-confirm-title"]');
  const button = (text: string, root: ParentNode = dialog() ?? document) => [...root.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === text)!;
  const choices = () => [...dialog()!.querySelectorAll<HTMLTableRowElement>(".sw-choices tbody tr")];
  const capture = async (name: string) => { await until(() => dialog() && getComputedStyle(dialog()!).opacity === "1"); await log(name); await sleep(550); };
  const named = (label: string) => dialog()!.querySelector<HTMLButtonElement>(`[aria-label="${label}"]`)!;
  try {
    await until(() => projectsPage().loaded && button("Swarm…", document) && !button("Swarm…", document).disabled);
    const origin = button("Swarm…", document); origin.focus(); origin.click();
    await until(() => dialog()?.open && button("Sync now")?.disabled === false);
    dialog()!.querySelector<HTMLButtonElement>("#swarm-conflicts-tab")!.click();
    await until(() => choices().length === (large ? 32 : 2) && dialog()!.querySelectorAll(".sw-clist li").length === (large ? 64 : 3));
    need(button("Choose a value above").disabled, "opening chose a value");
    need(!!dialog()!.querySelector(".sw-cpager") === large && !!dialog()!.querySelector(".sw-vpager") === large, "ordinary fixture acquired a pager or overflow omitted it");
    await capture("CONFLICTS");
    choices()[0].click(); await until(() => !!dialog()!.querySelector(".sw-choices tr.on") && button(large ? "Keep this value" : "Keep this project")?.disabled === false); await capture("CHOICE");
    if (large) {
      named("Next page of values").focus(); named("Next page of values").click();
      await until(() => dialog()!.querySelector(".sw-vpager")?.textContent?.includes("33–64 of 75") && document.activeElement === named("Next page of values"));
      need(!dialog()!.querySelector(".sw-choices tr.on") && button("Choose a value above").disabled, "page retained an invisible choice");
      named("Next page of values").click(); await until(() => dialog()!.querySelector(".sw-vpager")?.textContent?.includes("65–75 of 75") && document.activeElement === named("Previous page of values"));
      await capture("VALUES");
      const subject = dialog()!.querySelector(".sw-review > .row")!.textContent;
      named("Next page of conflicts").click(); await until(() => dialog()!.querySelector(".sw-cpager")?.textContent?.includes("65–128 of 130"));
      need(dialog()!.querySelector(".sw-review > .row")!.textContent === subject && !dialog()!.querySelector(".sw-clist .on"), "left paging changed the review");
      named("Next page of conflicts").click(); await until(() => dialog()!.querySelector(".sw-cpager")?.textContent?.includes("129–130 of 130") && document.activeElement === named("Previous page of conflicts"));
      named("Previous page of conflicts").click(); await until(() => dialog()!.querySelector(".sw-cpager")?.textContent?.includes("65–128 of 130"));
      named("Previous page of conflicts").click(); await until(() => dialog()!.querySelector(".sw-cpager")?.textContent?.includes("1–64 of 130") && document.activeElement === named("Next page of conflicts"));
      await capture("PAGERS");
      await log("RESIZE_MINIMUM"); await until(() => innerWidth <= 910 && innerHeight <= 570);
      const side = dialog()!.querySelector<HTMLElement>(".sw-cside")!, pager = dialog()!.querySelector<HTMLElement>(".sw-cpager")!;
      need(Math.abs(side.getBoundingClientRect().width - 210) < 1 && pager.scrollWidth <= side.clientWidth, "210px pager does not fit");
      await log("GEOMETRY " + JSON.stringify({ width: innerWidth, height: innerHeight, side: side.getBoundingClientRect().width, pager: pager.scrollWidth }));
      const pane = dialog()!.querySelector<HTMLElement>(".sw-review")!; pane.scrollTop = pane.scrollHeight;
      await capture("MINIMUM");
    } else {
      (dialog()!.querySelectorAll(".sw-clist li")[1] as HTMLElement).click(); await until(() => choices()[1]?.textContent?.includes("Remove folder link") && button("Choose a value above"));
      choices()[1].click(); await until(() => button("Keep this folder link") && button("Keep this folder link")?.disabled === false);
      const action = button("Keep this folder link"); action.focus(); action.click(); await until(() => confirmation()?.open && document.activeElement === button("Cancel", confirmation()!));
      need(confirmation()!.textContent?.includes("Files on disk stay intact."), "removal consequence missing"); await capture("CONFIRM");
      await log("ESCAPE_SWARM_CONFIRM"); await until(() => !confirmation() && document.activeElement === action); await capture("CANCEL");
      (dialog()!.querySelector(".sw-clist li") as HTMLElement).click(); await until(() => choices()[0]?.textContent?.toLowerCase().includes("sample") && button("Choose a value above"));
    }
    choices()[0].click(); await until(() => button(large ? "Keep this value" : "Keep this project")?.disabled === false);
    button(large ? "Keep this value" : "Keep this project").click();
    await until(() => dialog()!.querySelector('[role="status"]')?.textContent === "Saving your choice to the shared catalog…" && button("Close")?.disabled === false);
    need(button(large ? "Keep this value" : "Keep this project").disabled, "consumed review was enabled"); await capture("RESOLVE_HELD");
    button("Close").click(); await until(() => !dialog() && document.activeElement === origin);
    await log("PASS exact mockup Conflicts, native choices, paging/focus and confirmation, consumed Resolve with source barrier, Escape/focus and no replay");
    await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error) + " " + JSON.stringify({ dialogs: [...document.querySelectorAll("dialog")].map((d) => ({ label: d.getAttribute("aria-labelledby"), open: d.open })), active: document.activeElement?.outerHTML.slice(0, 180) }) + (error instanceof Error ? "\n" + error.stack : "")); await invoke("exit_app", { code: 1 }); }
}
