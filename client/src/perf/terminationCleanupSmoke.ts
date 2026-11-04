/** Private native walk; replay and an explicit fake hgs are mandatory. */
import { invoke } from "@tauri-apps/api/core";
import { catalog } from "../store/catalog";
import { actionEntries } from "../ui/sessions/ActionDialogs";
import { cleanup, openCleanup, cleanupDialog } from "../store/terminationCleanup";
import { notices } from "../store/notices";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
function require(ok: unknown, reason: string) { if (!ok) throw Error(reason); }
async function until(test: () => unknown, reason: string) { const deadline = performance.now() + 20_000; while (!test()) { if (performance.now() > deadline) throw Error(reason); await sleep(20); } }
export async function terminationCleanupSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "cleanup-smoke: " + line });
  const button = (root: string, text: string) => [...document.querySelectorAll<HTMLButtonElement>(root + " button")].find((b) => b.textContent?.trim() === text);
  async function ask(tag: string) {
    const row = Object.values(catalog.rows).find((s) => s.name === `codex/example/${tag}`)!;
    require(row, "synthetic session missing: " + tag);
    actionEntries(row).end.action();
    await until(() => document.querySelector("dialog[open] input[type=checkbox]"), "cleanup offer absent");
    return document.querySelector<HTMLInputElement>("dialog[open] input[type=checkbox]")!;
  }
  async function confirm(tag: string) {
    const checkbox = await ask(tag); if (!checkbox.checked) checkbox.click();
    button("dialog[open]", "Stop and remove folder")!.click();
  }
  try {
    await until(() => Object.values(catalog.rows).some((r) => r.name === "codex/example/clean"), "replay missing");
    const checkbox = await ask("clean"); require(!checkbox.checked, "fresh checkbox must default off");
    button("dialog[open]", "Cancel")!.click(); await sleep(150); require(!cleanup().rows.length, "Cancel created a cleanup job");
    await confirm("clean"); await until(() => cleanup().busy, "background cleanup did not begin");
    require(document.querySelector(".statusbar .chip-accent")?.isConnected, "statusbar did not render");
    await log("CLEANING"); await sleep(500);
    openCleanup(); await until(() => !!document.querySelector("dialog.worktree-cleanup[open]"), "review did not open while cleaning");
    require(button(".worktree-cleanup", "Keep folders")!.disabled, "Keep folders must be disabled during work");
    await log("ESCAPE_CLEANING"); await until(() => !cleanupDialog(), "Escape did not close the review");
    await until(() => !cleanup().busy && !cleanup().rows.length, "clean generated folder did not settle");
    require(notices().some((n) => n.title === "Folder removed"), "removed result needs its toast");
    await confirm("kept"); await until(() => cleanup().kept === 1 && !cleanup().busy, "local changes were not kept");
    await confirm("unknown"); await until(() => cleanup().unknown === 1 && !cleanup().busy, "uncertain removal did not stay unknown");
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
    document.querySelector<HTMLButtonElement>("dialog:has(.su-insp) button[aria-label=Close]")!.click();
    const kept = [...document.querySelectorAll<HTMLTableRowElement>(".bc-table tbody tr")].find((r) => r.title.endsWith("/kept"))!; kept.click();
    button(".worktree-cleanup", "Check again")!.click();
    await until(() => !!document.querySelector(".worktree-cleanup .solid-danger"), "Check again did not produce a ready target");
    const remove = document.querySelector<HTMLButtonElement>(".worktree-cleanup .solid-danger")!;
    require(remove.disabled, "uncommitted files were removable without consent");
    const discard = [...document.querySelectorAll<HTMLInputElement>(".bc-consent input")][0]; require(discard, "discard confirmation absent"); discard.click();
    require(!remove.disabled, "fresh explicit consent was not applied"); remove.click();
    await until(() => cleanup().rows.length === 1 && cleanup().unknown === 1 && !cleanup().busy, "manual cleanup did not settle without repeating unknown");
    button(".worktree-cleanup", "Keep folders")!.click(); await until(() => !cleanupDialog() && !cleanup().rows.length, "Keep folders did not dismiss recorded results");
    await confirm("unknown-only"); await until(() => cleanup().unknown === 1 && !cleanup().busy, "standalone unknown absent");
    await log("UNKNOWN"); await sleep(600);
    await confirm("changed"); await until(() => notices().some((n) => (n.text.includes("session state changed") || n.text === "Terminate cancelled: the session changed.")), "changed source did not refuse termination");
    require(cleanup().rows.length === 1 && cleanup().unknown === 1, "source change started cleanup");
    await log("PASS"); await sleep(200); await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error) + (error instanceof Error ? "\n" + error.stack : "")); await log("STATE " + JSON.stringify({ cleanup: cleanup(), notices: notices() })); await invoke("exit_app", { code: 1 }); }
}
