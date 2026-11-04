/** Private replay + fake hgs only. No real session or worktree is touched. */
import { invoke } from "@tauri-apps/api/core";
import { catalog } from "../store/catalog";
import { perform } from "../store/actions";
import { snapshotOf } from "../ui/sessions/actions-ui";
import { cleanup, cleanupDialog, closeCleanup } from "../store/terminationCleanup";
import { notices, sessionNoticeKey } from "../store/notices";
import { worktreeReview, worktreeReviewDialog, closeWorktreeReview } from "../store/worktreeCleanup";
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
function require(ok: unknown, reason: string) { if (!ok) throw Error(reason); }
async function until(test: () => unknown, reason: string) { const end = performance.now() + 20_000; while (!test()) { if (performance.now() > end) throw Error(reason); await sleep(20); } }
export async function archiveCleanupSmoke(allowed: boolean) {
  if (!allowed) return;
  const legacy = new URLSearchParams(location.search).get("archive-cleanup-smoke") === "legacy";
  const log = (line: string) => invoke("perf_log", { line: "archive-smoke: " + line });
  const session = (tag: string) => Object.values(catalog.rows).find((s) => s.name === `codex/example/${tag}`)!;
  const warning = (tag: string) => notices().find((n) => n.key === sessionNoticeKey(session(tag).computer, session(tag).name) && n.title === "Session archived");
  const toast = (tag: string) => document.querySelector<HTMLElement>(`.toast[data-notice-id="${warning(tag)?.id}"]`);
  const button = (root: ParentNode, text: string) => [...root.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === text)!;
  async function archive(tag: string) {
    const row = session(tag); require(row, "missing synthetic session " + tag);
    const out = await perform(snapshotOf(row), { kind: "archive" });
    if (out.outcome !== "done") throw Error(out.detail);
    require(out.cleanup_capture, "native archive capture missing");
    await until(() => warning(tag) && toast(tag), "archive recovery notice absent for " + tag);
    require(!toast(tag)!.textContent?.includes("/private/secret"), "CLI path list leaked into toast");
    require(button(toast(tag)!, "Review…"), "Review action absent");
    require(!!button(toast(tag)!, "Restore session") === !legacy, "Restore availability disagrees with CLI capability");
    return out.cleanup_capture!;
  }
  async function close() { closeCleanup(); await until(() => !document.querySelector("dialog.worktree-cleanup[open]"), "review did not close"); }
  try {
    await until(() => Object.values(catalog.rows).length === 66, "synthetic catalog incomplete");
    await archive("kept"); await log("NOTICE"); await sleep(600);
    const origin = button(toast("kept")!, "Review…"); origin.focus(); origin.click();
    await until(() => !!document.querySelector("dialog.worktree-cleanup[open]"), "recorded review did not open");
    require(document.querySelector(".bc-table tr.on")?.getAttribute("title")?.endsWith("/kept"), "Review did not select its recorded row");
    require(!document.querySelector(".worktree-cleanup .solid-danger"), "opening review offered removal without a check");
    require(document.querySelector(".worktree-cleanup .section-title")?.textContent === "Review 1 selected worktree", "single-row review lost singular copy");
    if (legacy) require(!button(document.querySelector("dialog.worktree-cleanup")!, "Restore session"), "legacy review offered guarded Restore");
    await log("REVIEW"); await sleep(500); await log("ESCAPE_REVIEW");
    await until(() => !cleanupDialog(), "Escape did not close recorded review");
    require(document.activeElement === origin, "Escape did not return focus to Review");
    if (legacy) {
      await log("LEGACY_PASS"); await invoke("exit_app", { code: 0 }); return;
    }
    origin.click(); await until(() => !!document.querySelector("dialog.worktree-cleanup[open]"), "recorded review did not reopen");
    button(document.querySelector("dialog.worktree-cleanup")!, "Restore session").click();
    await until(() => cleanup().busy, "restore did not disable cleanup controls");
    require(!cleanup().cleaning && cleanup().text !== "Cleaning working folders…", "restore pretended to clean the folder");
    require(button(toast("kept")!, "Review…").disabled && button(toast("kept")!, "Restore session").disabled, "restore left an action enabled");
    await log("RESTORING");
    await until(() => notices().some((n) => n.key === sessionNoticeKey(session("kept").computer, session("kept").name) && n.title === "Session restored"), "restore success notice missing");
    require(!cleanup().rows.length, "restored folder stayed in cleanup review");
    await log("RESTORED"); await sleep(500);

    await archive("unowned");
    require(warning("unowned")!.text === "Working folder kept for review: not created by Zerus.", "unowned precondition text differs");
    button(toast("unowned")!, "Review…").click(); await until(() => !!document.querySelector("dialog.worktree-cleanup[open]"), "unowned review missing");
    require(document.querySelector(".bc-table tr.on")?.textContent?.includes("Not created by Zerus; review the folder manually"), "unowned row lost its explicit reason");
    require(document.querySelector(".bc-table tr.on .bc-result b")?.textContent === "Review", "unowned row did not show Review");
    require(!document.querySelector(".worktree-cleanup .solid-danger"), "unowned row offered an unconfirmed removal");
    await log("UNOWNED"); await sleep(500); await close();

    await archive("unknown");
    button(toast("unknown")!, "Review…").click(); await until(() => !!document.querySelector("dialog.worktree-cleanup[open]"), "unknown review missing");
    require(button(document.querySelector("dialog.worktree-cleanup")!, "Check again").disabled, "unknown outcome offered retry");
    await log("UNKNOWN"); await sleep(500); await close();

    await archive("restore-refused"); button(toast("restore-refused")!, "Restore session").click();
    await until(() => notices().some((n) => n.tone === "error" && n.text === "The archived conversation changed; open Archive and refresh."), "existing restore error missing");
    require(!!warning("restore-refused"), "refusal hid its recovery notice");
    await log("REFUSED"); await sleep(500);
    await archive("changed"); button(toast("changed")!, "Restore session").click();
    await until(() => notices().some((n) => n.tone === "error" && n.text.includes("session state changed")), "changed archive identity was accepted");
    require(!!warning("changed"), "identity refusal hid its recovery notice");

    for (let n = 0; n < 60; ++n) await archive(`extra-${n}`);
    require(cleanup().rows.length === 64, "retained row bound changed");
    const overflow = await archive("extra-60"); require(!overflow.id && overflow.restore, "overflow lost its independent restore identity");
    require(cleanup().rows.length === 64 && cleanup().unknown === 2, "overflow was lost or grew retained rows");
    require(cleanup().title.includes("1 additional folders need review in Projects > Worktrees."), "overflow review sentence absent");
    button(toast("extra-60")!, "Review…").click();
    await until(() => !!worktreeReview() && !!document.querySelector("dialog.worktree-review[open]"), "overflow did not open the maintained Worktrees route");
    require(worktreeReview()!.focus.endsWith("/extra-60"), "overflow review lost the exact folder");
    await log("OVERFLOW"); await sleep(500); closeWorktreeReview(); await until(() => !worktreeReviewDialog(), "overflow review did not close");
    button(toast("extra-60")!, "Restore session").click();
    await until(() => cleanup().unknown === 1, "overflow restore did not remove its chip count");
    require(cleanup().rows.length === 64, "overflow restore changed unrelated retained rows");
    await log("PASS"); await sleep(200); await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error) + (error instanceof Error ? "\n" + error.stack : "")); await log("STATE " + JSON.stringify({ cleanup: cleanup(), notices: notices() })); await invoke("exit_app", { code: 1 }); }
}
