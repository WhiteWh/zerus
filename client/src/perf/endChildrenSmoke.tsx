// Native WebKit regression for zerus-sa9qn.17: the Terminate confirmation of a
// parent with live child sessions lists them and offers to end them too, off
// by default; `?kind=archive` opens Move to archive of a stopped parent, which
// asks only because it has live children. The real menu entry opens the real
// dialog over synthetic catalog rows. Served by Vite, outside the app entry;
// every native call is refused, and nothing is confirmed. Run:
//   npx vite --port <free port> --strictPort, then
//   python3 scripts/menu-smoke.py --title "Zerus end children regression (synthetic)" \
//     --url http://127.0.0.1:<port>/scripts/end-children-smoke.html
// in its own private Xvfb.
import { render } from "solid-js/web";
import { ActionDialogs, actionEntries } from "../ui/sessions/ActionDialogs";
import { applyBatch, catalog } from "../store/catalog";
import { applyMachines } from "../store/machines";
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

const archive = new URLSearchParams(location.search).get("kind") === "archive";
const MACHINE = "11111111-1111-4111-8111-111111111111";
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
const failures: string[] = [];
const check = (condition: unknown, message: string) => { if (!condition) failures.push(message); };
const batchOf = (part: Partial<Batch>): Batch => ({ ...emptyBatch(), ...part });
const now = Math.floor(Date.now() / 1000);
const link = (n: number, name: string): ParentLinkOut =>
  ({ machine: MACHINE, session_id: id(n), name, conversation_id: `c${n}`, run_id: `r${n}`, state: "live" });

function view(n: number, name: string, extra: Partial<ViewRow> = {}): ViewRow {
  return {
    name, title: name.slice(name.lastIndexOf("/") + 1), agent: name.slice(0, name.indexOf("/")), machine: "workstation", project: "zerus",
    folder: `/srv/zerus/${n}`, cwd: `/srv/zerus/${n}`, branch: "main", model: "example-model", state: "running", tracked: true, run_id: `r${n}`,
    conversation_id: `c${n}`, session_id: id(n), updated_at: now - n * 60, status: "ready", phase: "idle", activity: "idle", resumable: true,
    process_state: "running", git_worktree: false, summary: `Example summary ${n}`, ...extra,
  } as ViewRow;
}
const lead = "kimi/zerus/i3-coordinator";
const stopped = { state: "stopped", process_state: "exited" } as Partial<ViewRow>;
const rows = [
  { computer: "", key: "run:lead", row: view(1, lead, archive ? stopped : {}) },
  { computer: "", key: "run:zero", row: view(2, "kimi/zerus/i3-zero-native", { parent: link(1, lead), ...(archive ? stopped : {}) }) },
  { computer: "", key: "run:batch", row: view(3, "kimi/zerus/i3-batch-repair", { parent: link(1, lead), ...(archive ? stopped : {}) }) },
  { computer: "", key: "run:review", row: view(4, "claude/zerus/i3-review", { parent: link(3, "kimi/zerus/i3-batch-repair") }) },
  { computer: "", key: "run:saved", row: view(5, "codex/zerus/i3-saved-probe", { parent: link(1, lead), ...(archive ? {} : stopped) }) },
  { computer: "", key: "run:other", row: view(6, "codex/zerus/main-review") },
];

async function main() {
  applySkin(skins[0]);
  render(() => <div class="app"><ActionDialogs /></div>, document.getElementById("root")!);
  applyMachines(batchOf({ machines: [{ computer: "", machine: { machine_id: MACHINE } as never }] }));
  applyBatch(batchOf({ computers: [{ computer: "", status: "live", attempt: 0 }], replaces: [{ computer: "", rows }] }));
  for (let i = 0; i < 3; i++) await frame();
  const parent = catalog.rows["\nrun:lead"];
  check(parent, "the parent is listed");
  const entries = actionEntries(parent!);
  if (archive) entries.archive?.action(); else entries.end.action();
  for (let i = 0; i < 6; i++) await frame();
  const dialog = document.querySelector<HTMLDialogElement>("dialog.dlg");
  check(dialog?.open, "the confirmation is open");
  const title = dialog?.querySelector("#action-dlg-title")?.textContent;
  check(title === (archive ? "Move to archive?" : "Terminate session?"), `title: ${title}`);
  const box = dialog?.querySelector<HTMLInputElement>('.dlg-child-sessions input[type="checkbox"]');
  check(box && !box.checked, "the opt-in is there and off by default");
  const label = box?.parentElement?.textContent?.trim();
  check(label === (archive ? "Also archive 2 child sessions" : "Also end 3 child sessions"), `opt-in: ${label}`);
  const listed = [...(dialog?.querySelectorAll(".dlg-child-sessions li") ?? [])].map((li) => li.textContent);
  const expected = archive
    ? ["kimi/zerus/i3-zero-native", "kimi/zerus/i3-batch-repair", "codex/zerus/i3-saved-probe", "claude/zerus/i3-review"]
    : ["kimi/zerus/i3-zero-native", "kimi/zerus/i3-batch-repair", "claude/zerus/i3-review", "codex/zerus/i3-saved-probe"];
  check(JSON.stringify(listed) === JSON.stringify(expected), `children: ${JSON.stringify(listed)}`);
  check(!listed.includes("codex/zerus/main-review"), "an unrelated session is never listed");
  const stays = [...(dialog?.querySelectorAll(".dlg-child-sessions p.hint") ?? [])].map((p) => p.textContent);
  check(stays.some((t) => t === (archive ? "2 child sessions cannot be archived now and stay as they are:" : "1 child session cannot be terminated now and stays as it is:")), `stay text: ${JSON.stringify(stays)}`);
  const report = { failures, title, label, listed, stays };
  (window as unknown as { webkit: { messageHandlers: { result: { postMessage: (text: string) => void } } } }).webkit.messageHandlers.result.postMessage(JSON.stringify(report));
}
main().catch((error) => {
  (window as unknown as { webkit: { messageHandlers: { result: { postMessage: (text: string) => void } } } }).webkit.messageHandlers.result.postMessage(JSON.stringify({ failures: [String(error)] }));
});
