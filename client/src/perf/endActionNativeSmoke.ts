/** Private replay/fake hgs only. Exercises the native seam before UI wiring. */
import { invoke } from "@tauri-apps/api/core";
import { catalog } from "../store/catalog";
import type { Outcome, Target } from "../types";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
function require(value: unknown, text: string) { if (!value) throw Error(text); }
export async function endActionNativeSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "undo-native: " + line });
  const source = (tag: string): Target => {
    const row = Object.values(catalog.rows).find((r) => r.name === `codex/example/${tag}`)!;
    require(row, "synthetic row missing");
    return { computer: row.computer, name: row.name, run_id: row.runId ?? "", conversation_id: row.conversationId ?? "" };
  };
  const begin = (id: string, target: Target) => invoke("action_delay_begin", { id, captures: [{ target, action: { kind: "archive" }, state: "paused" }] });
  const run = (id: string, target: Target) => invoke<Outcome>("run_action", { target, action: { kind: "archive" }, delay: { id, index: 0 } });
  try {
    const until = performance.now() + 20_000;
    while (Object.keys(catalog.rows).length !== 66) { require(performance.now() < until, "catalog incomplete"); await sleep(20); }
    const kept = source("kept");
    let id = crypto.randomUUID(); await begin(id, kept);
    let early = false; try { await run(id, kept); } catch { early = true; }
    require(early, "native reservation executed early");
    const cancelled = invoke<boolean>("action_delay_wait", { id });
    await invoke("action_delay_cancel", { id }); require(!await cancelled, "Undo did not cancel wait");
    let replay = false; try { await run(id, kept); } catch { replay = true; }
    require(replay, "cancelled reservation was reused");

    id = crypto.randomUUID(); const started = performance.now(); await begin(id, kept);
    const ready = invoke<boolean>("action_delay_wait", { id }); await log("NATIVE"); await sleep(400);
    let returned = false;
    const away = invoke("perf_away", { how: "hide", window_ms: 6_000 }).then(() => { returned = true; });
    require(await ready, "hidden window lost its deadline"); require(performance.now() - started >= 5_000, "deadline released early");
    const out = await run(id, kept); require(out.outcome === "done", JSON.stringify(out));
    require(!returned, "action waited for window visibility instead of its native deadline");
    await log("EXECUTED_HIDDEN"); await away; await log("RESTORED"); await sleep(500);

    const changed = source("changed"); id = crypto.randomUUID(); await begin(id, changed);
    require(await invoke<boolean>("action_delay_wait", { id }), "fresh-source wait did not finish");
    const refused = await run(id, changed); require(refused.outcome === "failed", "changed source was mutated");
    await log("SOURCE_CHANGED");
    const oversized = source("restore-refused"); id = crypto.randomUUID(); await begin(id, oversized);
    require(await invoke<boolean>("action_delay_wait", { id }), "oversize-source wait did not finish");
    const tooLarge = await run(id, oversized); require(tooLarge.outcome === "failed", "oversize source was accepted");
    await log("LIMIT_REFUSED");
    id = crypto.randomUUID(); await begin(id, source("extra-0"));
    void invoke<boolean>("action_delay_wait", { id }).catch(() => false);
    await log("QUIT_PENDING"); await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error)); await invoke("exit_app", { code: 1 }); }
}
