/**
 * Private native walk; replay and an explicit fake hgs are mandatory.
 * A replay never holds native ownership, so the Terminate cleanup offer must
 * refuse with the native-origin code and keep every folder. Review rows come
 * from the CLI's own cleanup report, as after any Terminate.
 */
import { invoke } from "@tauri-apps/api/core";
import { catalog } from "../store/catalog";
import { actionEntries } from "../ui/sessions/ActionDialogs";
import { cleanup, openCleanup, cleanupDialog } from "../store/terminationCleanup";
import { notices, sessionNoticeKey } from "../store/notices";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
function require(ok: unknown, reason: string) { if (!ok) throw Error(reason); }
async function until(test: () => unknown, reason: string) { const deadline = performance.now() + 20_000; while (!test()) { if (performance.now() > deadline) throw Error(reason); await sleep(20); } }
export async function terminationCleanupSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "cleanup-smoke: " + line });
  const button = (root: string, text: string) => [...document.querySelectorAll<HTMLButtonElement>(root + " button")].find((b) => b.textContent?.trim() === text);
  const session = (tag: string) => Object.values(catalog.rows).find((s) => s.name === `codex/example/${tag}`)!;
  async function ask(tag: string) {
    const row = session(tag); require(row, "synthetic session missing: " + tag);
    actionEntries(row).end.action();
    await until(() => document.querySelector("dialog[open] .cleanup-offer-notice"), "cleanup offer refusal absent for " + tag);
    require(!document.querySelector("dialog[open] input[type=checkbox]"), "replay offered folder removal without native ownership");
    require(document.querySelector("dialog[open] .cleanup-offer-notice .dlg-error")?.textContent === "worktree_programs_native_origin_unavailable", "offer refusal lost its native-origin code");
    require(button("dialog[open]", "Review working folder…"), "manual review absent beside the refusal");
  }
  async function terminate(tag: string) {
    await ask(tag);
    button("dialog[open]", "Stop and archive")!.click();
    await until(() => !document.querySelector("dialog[open] .cleanup-offer-notice"), "Terminate dialog did not close");
  }
  const updated = (tag: string) => notices().some((n) => n.key === sessionNoticeKey(session(tag).computer, session(tag).name) && n.text === "Session updated. Refreshing…");
  try {
    await until(() => Object.values(catalog.rows).some((r) => r.name === "codex/example/clean"), "replay missing");
    await ask("clean"); await log("OFFER_REFUSED"); await sleep(400);
    button("dialog[open]", "Cancel")!.click(); await sleep(150); require(!cleanup().rows.length, "Cancel created a cleanup job");
    await terminate("clean"); await until(() => updated("clean"), "Terminate without cleanup did not finish");
    require(!cleanup().rows.length && !cleanup().busy, "Terminate without the offer started cleanup");
    await terminate("kept"); await until(() => cleanup().kept === 1 && !cleanup().busy, "CLI kept folder was not recorded");
    await terminate("unknown"); await until(() => cleanup().unknown === 1 && !cleanup().busy, "CLI unknown removal was not recorded");
    openCleanup(); await until(() => !!document.querySelector("dialog.worktree-cleanup[open]"), "review did not open");
    require(!button(".worktree-cleanup", "Close")!.disabled, "Close must dismiss review");
    await log("ESCAPE_REVIEW"); await until(() => !cleanupDialog(), "Escape did not close the review");
    require(cleanup().rows.length === 2, "Escape discarded recorded results");
    await log("MIXED"); await sleep(600);
    document.querySelector<HTMLButtonElement>(".statusbar button.warn-chip")!.click();
    await until(() => !!document.querySelector("dialog.worktree-cleanup[open]"), "chip did not reopen recorded review");
    const row = [...document.querySelectorAll<HTMLTableRowElement>(".bc-table tbody tr")].find((r) => r.title.endsWith("/unknown"))!; row.click();
    require(button(".worktree-cleanup", "Check again")!.disabled, "unknown outcome offered retry");
    require(!document.querySelector(".worktree-cleanup .solid-danger"), "recorded outcome offered removal without a check");
    await log("REOPENED"); await sleep(600);
    button(".worktree-cleanup", "View files and commits…")!.click();
    await until(() => !!document.querySelector(".su-insp .ip-files tbody tr"), "bounded read-only inspection missing");
    await log("INSPECTION"); await sleep(400);
    document.querySelector<HTMLButtonElement>("dialog.worktree-inspection button[aria-label=Close]")!.click();
    await until(() => !document.querySelector("dialog.worktree-inspection") && document.querySelector("dialog.worktree-cleanup[open]"), "review did not return after the inspection closed");
    const kept = [...document.querySelectorAll<HTMLTableRowElement>(".bc-table tbody tr")].find((r) => r.title.endsWith("/kept"))!; kept.click();
    // Selecting a kept row suspends the review while its blocker inspection loads.
    await until(() => document.querySelector("dialog.worktree-cleanup[open]") && button(".worktree-cleanup", "Check again"), "review did not stay open after selecting the kept row");
    button(".worktree-cleanup", "Check again")!.click();
    await until(() => !!document.querySelector(".worktree-cleanup .solid-danger"), "Check again did not produce a ready target");
    const remove = document.querySelector<HTMLButtonElement>(".worktree-cleanup .solid-danger")!;
    require(remove.disabled, "uncommitted files were removable without consent");
    const discard = [...document.querySelectorAll<HTMLInputElement>(".bc-consent input")][0]; require(discard, "discard confirmation absent"); discard.click();
    require(!remove.disabled, "fresh explicit consent was not applied"); remove.click();
    await until(() => cleanup().rows.length === 1 && cleanup().unknown === 1 && !cleanup().busy, "manual cleanup did not settle without repeating unknown");
    button(".worktree-cleanup", "Close")!.click(); await until(() => !cleanupDialog(), "Close did not dismiss review");
    require(cleanup().rows.length > 0, "Close discarded retained results");
    const held = cleanup().rows.length, unknown = cleanup().unknown;
    await terminate("unknown-only"); await until(() => cleanup().unknown === unknown + 1 && !cleanup().busy, "new unknown absent beside retained result");
    await log("UNKNOWN"); await sleep(600);
    await terminate("changed"); await until(() => notices().some((n) => n.text === "Terminate cancelled: the session changed."), "changed source did not refuse termination");
    require(cleanup().rows.length === held + 1 && cleanup().unknown === unknown + 1, "source change started cleanup");
    await log("PASS"); await sleep(200); await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error) + (error instanceof Error ? "\n" + error.stack : ""));
    await log("DIALOGS " + JSON.stringify([...document.querySelectorAll("dialog")].map((d) => `${d.className}${d.open ? " open" : ""}: ${[...d.querySelectorAll("button")].map((b) => b.textContent?.trim()).join("|")}`)));
    await log("STATE " + JSON.stringify({ cleanup: cleanup(), notices: notices() })); await invoke("exit_app", { code: 1 }); }
}
