// Native WebKit regression for zerus-sa9qn.17: an ended parent with live
// child sessions stays in the live Sessions list as their dimmed container.
// A stopped parent that the Active filter hides shows as Ended from its own
// row; a terminated (archived) parent shows as Archived from its children's
// link. Served by Vite, outside the app entry; every native read is refused. Run:
//   npx vite --port <free port> --strictPort, then
//   python3 scripts/menu-smoke.py --title "Zerus ended parent regression (synthetic)" \
//     --url http://127.0.0.1:<port>/scripts/ended-parent-smoke.html
// in its own private Xvfb.
import { render } from "solid-js/web";
import { SessionList } from "../ui/sessions/SessionList";
import { applyBatch } from "../store/catalog";
import { applyMachines } from "../store/machines";
import { setSessionsWidth, setStateFilter, setStripMode } from "../store/ui";
import { emptyBatch, type Batch, type ParentLinkOut, type ViewRow } from "../types";
import { applySkin, skins } from "../skin/runtime";
import "../ui/styles/base.css";
import "../ui/styles/app.css";
import "../ui/styles/sessions.css";
import "../ui/styles/detail.css";
import "../ui/styles/client.css";
import "../ui/styles/motion.css";

const native = window as unknown as { __TAURI_INTERNALS__?: { invoke: (command: string) => Promise<unknown> } };
native.__TAURI_INTERNALS__ = { invoke: (command) => Promise.reject(new Error(`synthetic smoke refuses ${command}`)) };

const MACHINE = "11111111-1111-4111-8111-111111111111";
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
const failures: string[] = [];
const check = (condition: unknown, message: string) => { if (!condition) failures.push(message); };
const batchOf = (part: Partial<Batch>): Batch => ({ ...emptyBatch(), ...part });
const now = Math.floor(Date.now() / 1000);
const link = (n: number, name: string, state: string): ParentLinkOut =>
  ({ machine: MACHINE, session_id: id(n), name, conversation_id: `c${n}`, run_id: `r${n}`, state });

function view(n: number, name: string, extra: Partial<ViewRow> = {}): ViewRow {
  const agent = name.slice(0, name.indexOf("/"));
  return {
    name, title: name.slice(name.lastIndexOf("/") + 1), agent, machine: "workstation", project: "zerus", folder: `/srv/zerus/${n}`,
    cwd: `/srv/zerus/${n}`, branch: "main", model: "example-model", state: "running", tracked: true, run_id: `r${n}`,
    conversation_id: `c${n}`, session_id: id(n), updated_at: now - n * 60, status: "ready", phase: "idle", activity: "idle",
    summary: `Example summary ${n}`, ...extra,
  } as ViewRow;
}
const working = { status: "working", phase: "working", activity: "busy", turn_started: now - 40 } as Partial<ViewRow>;
const rows = [
  { computer: "", key: "run:main", row: view(1, "codex/zerus/main-review", working) },
  // A coordinator stopped by the person: Active hides it, its children stay live.
  { computer: "", key: "run:coordinator", row: view(2, "kimi/zerus/i3-coordinator", { state: "stopped", status: "ready" }) },
  { computer: "", key: "run:zero", row: view(3, "kimi/zerus/i3-zero-native", { ...working, parent: link(2, "kimi/zerus/i3-coordinator", "saved") }) },
  { computer: "", key: "run:batch", row: view(4, "kimi/zerus/i3-batch-repair", { ...working, parent: link(2, "kimi/zerus/i3-coordinator", "saved") }) },
  // A terminated lead: archived on this computer, its worker still running.
  { computer: "", key: "run:worker", row: view(6, "claude/zerus/review-worker", { ...working, parent: link(5, "claude/zerus/review-lead", "archived") }) },
];

async function main() {
  applySkin(skins[0]);
  // The full list panel at a usual width, never the narrow strip.
  setStripMode(false); setSessionsWidth(420);
  // The app grid: the rail column first, so the list takes the page column as in the app.
  render(() => <div class="app"><nav class="rail" /><main class="page"><div class="sessions-page"><SessionList /></div></main></div>, document.getElementById("root")!);
  applyMachines(batchOf({ machines: [{ computer: "", machine: { machine_id: MACHINE } as never }] }));
  applyBatch(batchOf({ computers: [{ computer: "", status: "live", attempt: 0 }], replaces: [{ computer: "", rows }] }));
  setStateFilter("active");
  for (let i = 0; i < 6; i++) await frame();
  const boxes = [...document.querySelectorAll<HTMLElement>(".vrow[data-type='container']")];
  const titles = boxes.map((el) => `${el.querySelector(".sc-title")?.textContent} ${el.querySelector(".sbadge")?.textContent?.trim()}`);
  check(titles.includes("i3-coordinator Ended"), `the stopped coordinator is an Ended container: ${JSON.stringify(titles)}`);
  check(titles.includes("review-lead Archived"), `the terminated lead is an Archived container: ${JSON.stringify(titles)}`);
  check(boxes.every((el) => el.querySelector(".scard.ended-parent")), "every container is the dimmed ended-parent row");
  const cards = [...document.querySelectorAll<HTMLElement>(".vrow[data-type='card']")].map((el) => el.querySelector(".sc-title")?.textContent);
  check(["i3-zero-native", "i3-batch-repair", "review-worker", "main-review"].every((t) => cards.includes(t)), `every live session is listed: ${JSON.stringify(cards)}`);
  check(!cards.includes("i3-coordinator"), "the stopped coordinator is not a session card under Active");
  const nested = [...document.querySelectorAll<HTMLElement>(".vrow[data-type='card'] .rowhit.nested")].map((el) => el.querySelector(".sc-title")?.textContent);
  check(["i3-zero-native", "i3-batch-repair", "review-worker"].every((t) => nested.includes(t)), `the children nest under their containers: ${JSON.stringify(nested)}`);
  check(!nested.includes("main-review"), "a session without a parent stays at top level");
  check(!document.querySelector(".vrow[data-type='card'] .sc-from"), "nested children carry no from PARENT chip");
  const report = { failures, containers: titles, cards };
  (window as unknown as { webkit: { messageHandlers: { result: { postMessage: (text: string) => void } } } }).webkit.messageHandlers.result.postMessage(JSON.stringify(report));
}
main().catch((error) => {
  (window as unknown as { webkit: { messageHandlers: { result: { postMessage: (text: string) => void } } } }).webkit.messageHandlers.result.postMessage(JSON.stringify({ failures: [String(error)] }));
});
