/** Private built-app walk: an isolated replay and explicit fake hgs only. */
import { invoke } from "@tauri-apps/api/core";
import { catalog } from "../store/catalog";
import { setSelectedKey, setPage } from "../store/ui";
import { openInspector } from "../store/inspector";
import { lastCopied } from "../store/actions";
import { newSession } from "../store/newSession";
import { worktreeReview, worktreeReviewDialog, worktreeReviewLoading } from "../store/worktreeCleanup";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
function require(ok: unknown, why: string) { if (!ok) throw Error(why); }
async function until(test: () => unknown, why: string) { const deadline = performance.now() + 20_000; while (!test()) { if (performance.now() > deadline) throw Error(why); await sleep(20); } }
export async function worktreeReviewSmoke(allowed: boolean, projects = false, native = false, overviewUI = false) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "worktree-review-smoke: " + line });
  const button = (root: string, label: string) => [...document.querySelectorAll<HTMLButtonElement>(root + " button")].find((b) => b.textContent?.trim() === label);
  const scope = projects ? ".wtp" : ".inspector";
  const cleanupButton = () => [...document.querySelectorAll<HTMLButtonElement>(scope + " button")].find((b) => /^Clean up(?: \(\d+\))?…$/u.test(b.textContent?.trim() ?? ""));
  const menu = async (path: string) => { const card = [...document.querySelectorAll<HTMLElement>(projects ? ".wtp-root" : ".icard.wt")].find((c) => c.querySelector(projects ? ".wtp-path" : ".wt-path")?.textContent?.includes(path))!; require(card, "missing checkout card " + path); const r = card.getBoundingClientRect(); (projects ? card.querySelector<HTMLElement>(".wtp-row")! : card).dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: r.left + 50, clientY: r.top + 30 })); await until(() => document.querySelector(".menu[role=menu]"), "context menu missing"); };
  const choose = (names: string[]) => { for (const row of document.querySelectorAll<HTMLTableRowElement>(".wc-table tbody tr")) { const input = row.querySelector<HTMLInputElement>("input")!; const wanted = names.some((n) => row.title.endsWith("/" + n)); if (!input.disabled && input.checked !== wanted) input.click(); } };
  try {
    await until(() => Object.values(catalog.rows).some((s) => s.name === "codex/example/probe"), "synthetic session missing");
    const s = Object.values(catalog.rows).find((s) => s.name === "codex/example/probe")!;
    setSelectedKey(s.key); if (projects) setPage("projects"); else openInspector("worktrees");
    await until(() => document.querySelectorAll(projects ? ".wtp-root" : ".icard.wt").length >= 4 && !cleanupButton()?.disabled, "worktree panel did not load");
    if (projects) {
      await until(() => cleanupButton()?.textContent?.trim() === `Clean up (${overviewUI ? 304 : 4})…`, "read-only cleanup reminder count missing");
      require(cleanupButton()!.title.includes("Review them first."), "reminder lost its review-first tooltip");
    }
    await log("PANEL"); await sleep(500);
    if (overviewUI) {
      button(".projects .su-head", "Worktrees…")!.click();
      const rows = () => [...document.querySelectorAll<HTMLElement>(".worktree-overview .wto-row[data-row]")];
      const row = (name: string) => rows().find((r) => r.querySelector("b")?.textContent === name)!;
      const next = () => document.querySelector<HTMLButtonElement>('[aria-label="Next page of worktrees"]')!;
      const previous = () => document.querySelector<HTMLButtonElement>('[aria-label="Previous page of worktrees"]')!;
      await until(() => rows().length === 256 && next() && !next().disabled, "overview did not page the bounded catalog");
      require(document.querySelector(".wto-left > .wto-pager") && !document.querySelector(".dlg-foot .wto-pager"), "pager placement changed");
      require(button(".worktree-overview", "Select candidates")!.disabled, "more than64 candidates selected a prefix");
      await log("OVERVIEW"); await sleep(1500);
      const clean = row("clean"); require(clean, "clean checkout missing"); clean.click();
      await until(() => clean.getAttribute("aria-selected") === "true", "native selection missing");
      require(row("clean") === clean, "selection recreated the worktree row");
      next().click(); await until(() => previous() && !previous().disabled && next().disabled && rows().length === 48, "last page missing");
      await until(() => document.activeElement === rows()[0], "last-page button did not hand focus to first row");
      rows()[1].dispatchEvent(new MouseEvent("click", { bubbles: true, ctrlKey: true }));
      await until(() => button(".worktree-overview", "Review cleanup (2)…"), "cross-page selection lost");
      previous().click(); await until(() => previous().disabled && rows().length === 256, "first page missing");
      require(row("clean").getAttribute("aria-selected") === "true", "selection did not survive page navigation");
      await log("PAGER"); await sleep(1500);
      const search = document.querySelector<HTMLInputElement>('.worktree-overview input[type="search"]')!;
      search.value = "clean"; search.dispatchEvent(new InputEvent("input", { bubbles: true }));
      await until(() => rows().length === 1 && !document.querySelector(".wto-pager") && !button(".worktree-overview", "Select candidates")!.disabled, "filter did not reset/clamp paging");
      button(".worktree-overview", "Select candidates")!.click(); await until(() => button(".worktree-overview", "Review cleanup (1)…"), "filtered candidate selection not atomic");
      search.value = "unknown"; search.dispatchEvent(new InputEvent("input", { bubbles: true })); await until(() => row("unknown") && rows().length === 1, "unknown target not filtered");
      row("unknown").dispatchEvent(new MouseEvent("click", { bubbles: true, ctrlKey: true })); await until(() => button(".worktree-overview", "Review cleanup (2)…") && !button(".worktree-overview", "Review cleanup (2)…")!.disabled, "second selected target missing");
      button(".worktree-overview", "Review cleanup (2)…")!.click(); await until(() => document.querySelector(".worktree-manual") && button(".worktree-manual", "Remove 2 folders") && !button(".worktree-manual", "Remove 2 folders")!.disabled, "explicit fresh batch review missing");
      require(!document.querySelector<HTMLInputElement>(".worktree-manual .bc-consent input")!.checked, "branch consent remembered");
      await log("MANUAL_REVIEW"); await sleep(1500);
      button(".worktree-manual", "View files and commits…")!.click(); await until(() => document.querySelector("#cleanup-inspect-title") && !document.querySelector('.dlg:has(#cleanup-inspect-title) .ip-state')?.textContent?.includes("Loading"), "bounded inspector missing");
      button(".dlg:has(#cleanup-inspect-title)", "Local")!.click(); await until(() => document.querySelector('.dlg:has(#cleanup-inspect-title) .ip-files tbody tr'), "local listing absent");
      document.querySelector<HTMLElement>('.dlg:has(#cleanup-inspect-title) .ip-files tbody tr')!.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
      await until(() => document.querySelector('.dlg:has(#cleanup-inspect-title) .ip-diff')?.textContent?.includes("Synthetic fixture preview"), "local bounded preview absent");
      const inspectedDialog = document.querySelector<HTMLDialogElement>('.dlg:has(#cleanup-inspect-title)')!;
      await until(() => getComputedStyle(inspectedDialog).opacity === "1", "inspection did not finish opening");
      const rect = inspectedDialog.getBoundingClientRect();
      await log("INSPECTION_GEOMETRY " + JSON.stringify({ open: inspectedDialog.open, top: rect.top, bottom: rect.bottom, height: innerHeight }));
      require(inspectedDialog.open && rect.top >= 0 && rect.bottom <= innerHeight, "inspection dialog did not render inside the window");
      await log("INSPECTION"); await sleep(5000); document.querySelector<HTMLButtonElement>('.dlg:has(#cleanup-inspect-title) button[aria-label="Close"]')!.click();
      document.querySelector<HTMLInputElement>(".worktree-manual .bc-consent input")!.click();
      button(".worktree-manual", "Remove 2 folders")!.click();
      await until(() => document.querySelector(".worktree-manual .bc-summary")?.textContent?.includes("1 removed  0 kept  1 unknown"), "manual outcomes missing");
      require(button(".worktree-manual", "Check again")!.disabled, "unknown offered a retry");
      await log("MANUAL_OUTCOMES"); await sleep(1500); button(".worktree-manual", "Close")!.click();
      await until(() => !document.querySelector(".worktree-manual") && row("unknown")?.textContent?.includes("Result unknown"), "unknown was not retained in overview");
      button(".worktree-overview .dlg-foot", "Close")!.click(); await until(() => !document.querySelector(".worktree-overview"), "overview did not close");
      await until(() => document.activeElement === button(".projects .su-head", "Worktrees…"), "overview focus did not return");
      await log("PASS overview UI pages/selection/inspection/explicit consents/outcomes/focus"); await invoke("exit_app", { code: 0 }); return;
    }
    if (native) {
      const filter = { search: "", project: "", computer: "", category: null, show_main: false, sort: "size" };
      const opened = await invoke<{ id: string }>("worktree_overview_open", { all: false });
      type Row = { id: string; path: string; category: number; fresh: boolean };
      type Page = { reading: boolean; page: { total: number; groups: { rows: Row[] }[] } };
      const read = () => invoke<Page>("worktree_overview_page", { id: opened.id, filter, index: 0 });
      let page = await read();
      while (page.reading) { await sleep(20); page = await read(); }
      const rows = page.page.groups.flatMap((g) => g.rows);
      require(rows.length === 4, "opaque overview did not read the four linked checkouts");
      const chosen = rows.filter((r) => r.path.endsWith("/clean") || r.path.endsWith("/unknown"));
      await invoke("worktree_overview_select", { id: opened.id, ids: chosen.map((r) => r.id), replace: true });
      type Manual = { id: string; revision: number; busy: boolean; removed: number; unknown: number; rows: { ready: boolean }[] };
      let manual = await invoke<Manual>("worktree_overview_review", { id: opened.id });
      const settle = async () => { while (manual.busy) { await sleep(20); manual = await invoke<Manual>("worktree_manual_view", { id: manual.id }); } };
      await settle(); require(manual.rows.every((r) => r.ready), "manual fresh review refused synthetic rows");
      await log("NATIVE_REVIEW");
      manual = await invoke<Manual>("worktree_manual_remove", { id: manual.id, revision: manual.revision, consents: { local_files: false, discard_changes: false, unchecked_processes: false, merged_branches: true } });
      await settle(); require(manual.removed === 1 && manual.unknown === 1, "native manual outcomes lost");
      await invoke("worktree_manual_stop", { id: manual.id, close: true });
      await invoke("worktree_overview_refresh", { id: opened.id }); page = await read();
      while (page.reading) { await sleep(20); page = await read(); }
      const unknown = page.page.groups.flatMap((g) => g.rows).find((r) => r.path.endsWith("/unknown"))!;
      require(unknown?.category === 3 && !unknown.fresh, "unknown native result became a candidate");
      await invoke("worktree_overview_select", { id: opened.id, ids: [unknown.id], replace: true });
      manual = await invoke<Manual>("worktree_overview_review", { id: opened.id }); await settle();
      require(manual.unknown === 1 && !manual.rows[0].ready, "reopening rearmed unknown removal");
      await invoke("worktree_manual_stop", { id: manual.id, close: true });
      await invoke("worktree_overview_close", { id: opened.id });
      await log("PASS native opaque overview, fresh manual review, exact removal and terminal unknown");
      await invoke("exit_app", { code: 0 }); return;
    }
    await menu("/ignored"); button(".menu", "Copy path")!.click(); await until(() => lastCopied()?.endsWith("/ignored"), "Copy path failed");
    button(scope, "New session…")!.click(); await until(() => newSession()?.folderPath.endsWith("/ignored"), "New session did not retain selected checkout");
    require(!newSession()?.submitted, "opening New session created work"); document.querySelector<HTMLButtonElement>(".ns-dlg button[aria-label=Close]")!.click();
    await menu("/missing"); button(".menu", "Forget missing worktree…")!.click();
    await until(() => worktreeReview()?.focus.endsWith("/missing"), "missing worktree focus lost"); require(worktreeReview()?.rows.length === 1, "focused review escaped its checkout");
    button(".worktree-review", "Close")!.click(); await until(() => !worktreeReviewDialog(), "review did not close");
    await until(() => !cleanupButton()!.disabled, "catalog did not refresh after review");
    cleanupButton()!.click(); await until(() => worktreeReview()?.rows.length === 6, "full review missing");
    require(!document.querySelector<HTMLInputElement>(".worktree-review .check input")!.checked, "branch opt-in must default off");
    const blocked = [...document.querySelectorAll<HTMLTableRowElement>(".wc-table tbody tr")].find((r) => r.title.endsWith("/blocked"))!; require(blocked.querySelector<HTMLInputElement>("input")!.disabled, "known use offered removal");
    await log("REVIEW"); await sleep(500); choose(["ignored"]);
    button(".worktree-review", "Remove selected…")!.click(); await until(() => document.querySelector("#worktree-confirm-title"), "confirmation absent");
    require(document.querySelector<HTMLButtonElement>("dialog:has(#worktree-confirm-title) .solid-danger")!.disabled, "local files deleted without consent");
    require(document.querySelector("dialog:has(#worktree-confirm-title)")!.textContent!.includes("target/"), "ignored summary missing top paths");
    await log("CONFIRMATION"); await sleep(500); await log("ESCAPE_CONFIRM"); await until(() => !document.querySelector("#worktree-confirm-title") && !worktreeReviewLoading(), "Escape did not cancel confirmation");
    require(worktreeReview()?.rows.every((r) => !["Removed", "Unknown"].includes(r.state)), "Cancel had an effect");
    choose(["clean", "ignored", "missing", "unknown", "changed"]);
    document.querySelector<HTMLInputElement>(".worktree-review .check input")!.click();
    button(".worktree-review", "Remove 5 worktrees…")!.click(); await until(() => document.querySelector("#worktree-confirm-title"), "batch confirmation absent");
    const c = document.querySelector<HTMLDialogElement>("dialog:has(#worktree-confirm-title)")!;
    require(c.textContent!.includes("Merged branches deleted: clean, ignored, changed."), "confirmation did not name exact merged branches");
    c.querySelector<HTMLInputElement>(".check input")!.click(); c.querySelector<HTMLButtonElement>(".solid-danger")!.click();
    await until(() => worktreeReview()?.busy, "removal did not begin"); require(button(".worktree-review", "Close")!.disabled, "cleanup can be abandoned during removal");
    await log("REMOVING"); await sleep(500); await until(() => worktreeReview() && !worktreeReview()!.busy && worktreeReview()!.unknown, "outcomes did not settle");
    require(worktreeReview()!.rows.filter((r) => r.state === "Removed" || r.state === "Forgotten").length === 3, "success outcomes lost");
    require(worktreeReview()!.rows.some((r) => r.path.endsWith("/changed") && r.state === "Kept"), "stale fingerprint was not refused");
    await log("OUTCOMES"); await sleep(500); button(".worktree-review", "Review again")!.click();
    await until(() => worktreeReview()!.rows.some((r) => r.path.endsWith("/changed") && r.selectable), "verified refusal did not return to review");
    require(worktreeReview()!.rows.some((r) => r.path.endsWith("/unknown") && !r.selectable && r.state === "Unknown"), "unknown was rearmed by refresh");
    require(!document.querySelector<HTMLInputElement>(".worktree-review .check input")!.checked, "branch consent was remembered");
    await log("ESCAPE_REVIEW"); await until(() => !worktreeReviewDialog(), "Escape did not close the review");
    await until(() => document.activeElement === cleanupButton(), "review focus did not return");
    await until(() => !cleanupButton()!.disabled, "catalog not refreshed");
    if (projects) {
      await until(() => cleanupButton()?.textContent?.trim() === "Clean up (1)…", "changed review did not recount immediately");
      require(cleanupButton()!.title === "1 worktree can be removed without losing work. Review it first.", "recount lost its singular review-first tooltip");
      await log("RECOUNT");
    }
    await menu("/unknown"); button(".menu", "Remove worktree…")!.click();
    await until(() => worktreeReview()?.rows.length === 1 && worktreeReview()?.unknown, "unknown outcome not retained after reopening");
    require(button(".worktree-review", "Remove selected…")!.disabled && button(".worktree-review", "Review again")!.disabled, "unknown offered a retry");
    await log("UNKNOWN_REOPEN"); await sleep(500); button(".worktree-review", "Close")!.click();
    await log("PASS"); await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error) + (error instanceof Error ? "\n" + error.stack : "")); await log("STATE " + JSON.stringify(worktreeReview())); await invoke("exit_app", { code: 1 }); }
}
