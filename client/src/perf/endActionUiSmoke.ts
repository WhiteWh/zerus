/** Private replay/fake CLI only. Drives the shipped stores and painted Undo. */
import { invoke } from "@tauri-apps/api/core";
import { catalog } from "../store/catalog";
import { perform } from "../store/actions";
import { run as batchRun } from "../store/batch";
import { bus } from "../store/notices";
import { snapshotOf } from "../ui/sessions/actions-ui";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
function require(value: unknown, text: string) { if (!value) throw Error(text); }
async function until(f: () => boolean) { const end = performance.now() + 20_000; while (!f()) { require(performance.now() < end, "UI did not settle"); await sleep(20); } }
const undo = () => [...document.querySelectorAll<HTMLButtonElement>(".toasts button")].filter((b) => b.textContent?.startsWith("Undo ("));
const clear = () => { for (const n of bus.notices()) bus.dismiss(n.id); };
export async function endActionUiSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "undo-ui: " + line });
  try {
    await until(() => Object.keys(catalog.rows).length === 66);
    const row = (tag: string) => Object.values(catalog.rows).find((r) => r.name === `codex/example/${tag}`)!;
    const cancelled = perform(snapshotOf(row("kept")), { kind: "archive" });
    await until(() => undo().length === 1); require(undo()[0].textContent === "Undo (5)", "initial countdown");
    undo()[0].focus(); await log("UNDO"); await sleep(500); require(document.activeElement === undo()[0], "countdown replaced focused Undo"); undo()[0].click();
    require((await cancelled).outcome === "failed", "Undo ran archive"); clear();

    const pending = Array.from({ length: 5 }, (_, n) => perform(snapshotOf(row(`extra-${n}`)), { kind: "archive" }));
    await until(() => undo().length === 5);
    await perform(snapshotOf(row("extra-5")), { kind: "archive" });
    require(undo().length === 5 && document.querySelector(".toasts")!.textContent!.includes("Five actions are waiting. Undo one or wait a moment."), "sixth changed the stack");
    await log("FIVE"); await sleep(450); for (const button of undo()) button.click(); await Promise.all(pending); clear();

    const running = perform(snapshotOf(row("kept")), { kind: "archive" }); await until(() => undo().length === 1);
    await log("CLOSE"); await sleep(400); let returned = false;
    const away = invoke("perf_away", { how: "close", window_ms: 6000 }).then(() => { returned = true; });
    const result = await running; require(result.outcome === "done", JSON.stringify(result)); require(!returned, "close-to-tray cancelled or delayed the action");
    await log("EXECUTED_CLOSED"); await away; await log("RESTORED"); clear();

    const rows = [6, 7, 8].map((n) => row(`extra-${n}`));
    let batch = batchRun("archive", rows); await until(() => undo().length === 1);
    require(bus.notices().some((n) => n.title === "Archiving 3 sessions"), "batch split into separate windows");
    undo()[0].click(); await batch; clear();
    batch = batchRun("archive", rows); await until(() => undo().length === 1); await log("BATCH"); await batch;
    require(!undo().length && bus.notices().some((n) => n.text.includes("3 of 3 completed")), "batch did not settle"); clear();

    const changed = perform(snapshotOf(row("changed")), { kind: "archive" }); await until(() => undo().length === 1); await changed;
    require(bus.notices().some((n) => n.text === "Archive cancelled: the session changed."), "fresh identity refusal copy"); clear();
    const quitting = perform(snapshotOf(row("extra-9")), { kind: "archive" }); void quitting;
    await until(() => undo().length === 1); await log("QUIT_PENDING"); await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error)); await invoke("exit_app", { code: 1 }); }
}
