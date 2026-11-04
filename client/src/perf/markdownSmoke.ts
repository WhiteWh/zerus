import { invoke } from "@tauri-apps/api/core";
import { catalog } from "../store/catalog";
import { activity } from "../store/session";
import { notices } from "../store/notices";
import { setDetailTab, setPage, setSearch, setSelectedKey, setStateFilter } from "../store/ui";
import { INLINE_TABLE, SCREENSHOT_TABLE, WIDE_TABLE } from "./fixtures/markdownTables";

const COMMONMARK_REPLY = [
  "My proposal:", "- keep the [plan](docs/plan.md) short", "- ship it *carefully*", "  - with a nested check", "- ~~old step~~ dropped",
  "", "## Plan", "", "2. second", "3. third", "", "### Done so far", "", "- [x] parser", "- [ ] typography",
  "", "> Read it *carefully*.", "", "---", "", "Raw <img src=x onerror=alert(1)> stays text.",
].join("\n");

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(test: () => boolean, reason: string) {
  const deadline = performance.now() + 15_000;
  while (!test()) { if (performance.now() > deadline) throw Error(reason); await sleep(16); }
}
function require(ok: boolean, reason: string) { if (!ok) throw Error(reason); }

/** Requires the synthetic replay and explicit fake CLI; it never opens a link. */
export async function markdownSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "markdown-smoke: " + line });
  try {
    const key = "\nrun:000000-run";
    await until(() => !!catalog.rows[key], "replay catalog did not arrive");
    setPage("sessions"); setStateFilter("all"); setSearch(""); setSelectedKey(key); setDetailTab("activity");
    await until(() => activity.target()?.id === key && activity.loaded(), "initial replay Activity did not arrive");
    activity.applyActivity([{ computer: "", key: "run:000000-run", kind: "replace", payload: { events: [{
      seq: 1, type: "AgentMessage", at: 42,
      detail: ["Before the table.", SCREENSHOT_TABLE, "After the table.", INLINE_TABLE, WIDE_TABLE, "```md\n" + SCREENSHOT_TABLE + "\n```"].join("\n\n"),
    }] } }]);
    await until(() => document.querySelectorAll(".act-table").length === 3, "Activity did not render structured tables");
    await sleep(150);
    require(document.querySelectorAll(".act-table").length === 3, "a late replay page replaced the rendered fixture");
    const tables = [...document.querySelectorAll<HTMLTableElement>(".act-table")];
    const wrappers = [...document.querySelectorAll<HTMLElement>(".act-table-wrap")];
    require(tables[0].querySelectorAll("th").length === 2 && tables[0].querySelectorAll("tbody tr").length === 6, "Russian table lost headers or rows");
    require(tables[0].querySelector("td b")?.textContent === "Работает в dev", "bold cell text was lost");
    require(wrappers[0].scrollWidth <= wrappers[0].clientWidth + 1, "ordinary table escaped the message width");
    const last = tables[0].querySelector("tbody tr:last-child td:last-child")!;
    require(last.getBoundingClientRect().height > parseFloat(getComputedStyle(last).lineHeight) * 2, "the long last column did not wrap");
    require(wrappers[2].scrollWidth > wrappers[2].clientWidth + 1, "wide table has no horizontal scrolling");
    const align = ["left", "center", "right"];
    [...tables[1].querySelectorAll("th")].forEach((cell, index) => require(getComputedStyle(cell).textAlign === align[index], "header alignment changed"));
    [...tables[1].querySelectorAll("tbody tr:first-child td")].forEach((cell, index) => require(getComputedStyle(cell).textAlign === align[index], "cell alignment changed"));
    require(tables[1].querySelector("code")?.textContent === "a|b", "escaped code pipe was split into columns");
    require(!tables[1].querySelector("img,script,[onerror],a[href^='javascript:']"), "cell text became executable HTML");
    require(tables[1].querySelector("a")?.getAttribute("href") === "https://example.com/docs", "guarded external link was lost");
    require(tables[1].querySelectorAll("a")[1]?.getAttribute("href") === "docs/example.md", "guarded file link was lost");
    require(document.querySelector(".act-code")?.textContent === SCREENSHOT_TABLE, "a fenced table became live markup");
    const scroll = document.querySelector<HTMLElement>(".act-scroll")!;
    require(scroll.scrollWidth <= scroll.clientWidth + 1, "wide table expanded the whole Activity");
    const headerStyle = getComputedStyle(tables[0].querySelector("th")!), cellStyle = getComputedStyle(last);
    await log("STYLE " + JSON.stringify({ headerColor: headerStyle.color, headerFont: headerStyle.fontSize,
      divider: headerStyle.borderBottomColor, bodyFont: cellStyle.fontSize }));
    // CommonMark blocks (zerus-5wmh.66.2.35): the owner's list after a colon,
    // ordered, nested and task lists, headings, a quote, a rule, strikethrough.
    activity.applyActivity([{ computer: "", key: "run:000000-run", kind: "replace", payload: { events: [
      { seq: 3, type: "UserPromptSubmit", at: 41, detail: "What do you propose?" },
      { seq: 4, type: "AgentMessage", at: 42, detail: COMMONMARK_REPLY },
      { seq: 5, type: "UserPromptSubmit", at: 43, detail: "Thanks." },
    ] } }]);
    await until(() => !!document.querySelector(".act-body ol.md-list"), "CommonMark reply did not render");
    await sleep(150);
    const body = [...document.querySelectorAll<HTMLElement>(".act-body")].find((b) => b.querySelector("ol.md-list"))!;
    const first = body.firstElementChild!;
    require(first.tagName === "P" && first.textContent === "My proposal:" && !!first.nextElementSibling?.matches("ul.md-list"), "a list after a paragraph was glued into it");
    require(first.nextElementSibling!.querySelectorAll(":scope > li").length === 3, "the proposal list lost items");
    require(body.querySelector("ol.md-list")?.getAttribute("start") === "2", "an ordered list lost its start");
    require(!!body.querySelector("ul.md-list li ul.md-list"), "a nested list was flattened");
    require(body.querySelectorAll("li.md-task input[type=checkbox][disabled]").length === 2 && !!body.querySelector("li.md-task input:checked"), "task items lost their boxes");
    require(body.querySelector("h2.md-heading")?.textContent === "Plan" && !!body.querySelector("h3.md-heading"), "headings were not recognized");
    require(body.querySelector("blockquote.md-quote em")?.textContent === "carefully", "a quote or its emphasis was lost");
    require(body.querySelector("del")?.textContent === "old step" && !!body.querySelector("hr.md-rule"), "strikethrough or a rule was lost");
    require(!body.querySelector("img,script,b[onclick]") && body.textContent!.includes("<img src=x onerror=alert(1)>"), "raw HTML became live markup");
    require(body.querySelector("a")?.getAttribute("href") === "docs/plan.md", "a file reference was lost");
    // Rows and the reply's blocks never overlap: each starts below the one before.
    const rows = [...document.querySelectorAll<HTMLElement>(".act-col > [id^='item-']")];
    for (let i = 1; i < rows.length; i++) require(rows[i].getBoundingClientRect().top >= rows[i - 1].getBoundingClientRect().bottom - 0.5, "Activity rows overlap");
    const parts = [...body.children] as HTMLElement[];
    for (let i = 1; i < parts.length; i++) require(parts[i].getBoundingClientRect().top >= parts[i - 1].getBoundingClientRect().bottom - 0.5, "message blocks overlap");
    await log("COMMONMARK_SHOWN " + JSON.stringify({ blocks: parts.map((part) => part.tagName.toLowerCase()), rows: rows.length }));
    await sleep(2500);
    // Leave the owner's reported table on screen for the built-app capture.
    activity.applyActivity([{ computer: "", key: "run:000000-run", kind: "replace", payload: { events: [{
      seq: 2, type: "AgentMessage", at: 42, detail: SCREENSHOT_TABLE,
    }] } }]);
    await until(() => document.querySelectorAll(".act-table").length === 1, "reported table capture did not settle");
    await sleep(150);
    const header = document.querySelector(".act-table th")!;
    require(header.getBoundingClientRect().top >= scroll.getBoundingClientRect().top, "the reported table header is outside the viewport");
    const beforeCopy = Math.max(0, ...notices().map((n) => n.id));
    const copy = document.querySelector<HTMLButtonElement>(".act-copy");
    require(!!copy, "Copy as Markdown control is missing");
    copy!.click();
    await until(() => notices().some((n) => n.id > beforeCopy), "copy did not report its result");
    require(notices().some((n) => n.id > beforeCopy && n.title === "Copied" && n.tone !== "error"), "Copy as Markdown was refused");
    await log("COPIED (exact multiline table through the native clipboard bridge)");
    await log("PASS (CommonMark lists after text, ordered/nested/task lists, headings, quote, rule, strikethrough, no overlap; Russian rows, wrapping, wide scrolling, alignment, bold/code/guarded links, escaped pipes, literal HTML and fences)");
    await sleep(500);
    await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error) + (error instanceof Error ? "\n" + (error.stack ?? "") : "")); await invoke("exit_app", { code: 1 }); }
}
