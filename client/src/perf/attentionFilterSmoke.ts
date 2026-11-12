import { invoke } from "@tauri-apps/api/core";
import { applyBatch, catalog } from "../store/catalog";
import { filterCounts, listRows } from "../ui/sessions/model";
import { setAccountFilter, setFolderFilter, setPage, setProjectFilter, setSearch, setSelectedKey, setStateFilter, type StateFilter } from "../store/ui";
import { emptyBatch, type MachineMeta, type ViewRow } from "../types";

// zerus-sa9qn.18: the owner's case in the running app. A supervising parent's
// quiet children (unread reply, waiting to retry, blocked by the provider), a
// child with a request only the person can answer, orphans and a muted
// session; each status tab must list exactly what its count counts. Only the
// fake CLI and synthetic replay can run it (attention-filter-smoke.py).

const MACHINE = "11111111-1111-4111-8111-111111111111";
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(test: () => boolean, reason: string) {
  const deadline = performance.now() + 15_000;
  while (!test()) { if (performance.now() > deadline) throw Error(reason); await sleep(16); }
}
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
function view(n: number, title: string, extra: Partial<ViewRow> = {}, parent?: number): ViewRow {
  return { name: `claude/fixture/s${n}`, title, agent: "claude", machine: "workstation", status: "ready", activity: "idle", state: "running",
    process_state: "running", tracked: true, session_id: id(n), run_id: `r${n}`, conversation_id: `c${n}`, needs_attention: false,
    needs_action: false, parent: parent === undefined ? undefined : { machine: MACHINE, session_id: id(parent), name: `claude/fixture/s${parent}`,
      conversation_id: `c${parent}`, run_id: `r${parent}`, state: "live" }, ...extra } as ViewRow;
}
const UNREAD = { status: "ready", unread: true, needs_attention: true } as const;
const RETRY = { status: "error", caption: "Waiting to retry", needs_attention: true, needs_action: true } as const;
const BLOCKED = { status: "error", caption: "Request blocked by provider", needs_attention: true, needs_action: true } as const;
const ASKS = { status: "attention", phase: "approval", caption: "Approval needed", needs_attention: true, needs_action: true } as const;

export const ATTENTION_FIXTURES: ViewRow[] = [
  view(1, "Lead: supervises its children", { status: "working", activity: "busy" }),
  view(2, "Quiet child: unread reply", { ...UNREAD, quiet: true }, 1),
  view(3, "Quiet child: waiting to retry", { ...RETRY, quiet: true }, 1),
  view(4, "Quiet child: blocked by provider", { ...BLOCKED, quiet: true }, 1),
  view(5, "Quiet child: working", { status: "working", activity: "busy", quiet: true }, 1),
  view(6, "Child asks the person: approval", { ...ASKS, quiet: false }, 1),
  view(7, "Orphan quiet child: unread reply", { ...UNREAD, quiet: true }, 99),
  view(8, "Orphan child asks the person", { ...ASKS }, 98),
  view(9, "Idle parent", {}),
  view(10, "Child of an idle parent asks", { ...ASKS }, 9),
  view(11, "Quiet child of an idle parent: retry", { ...RETRY, quiet: true }, 9),
  view(12, "Muted session: unread reply", { ...UNREAD, muted: true }),
  view(13, "Unread reply", { ...UNREAD }),
];

/** The listed cards' short fixture ids (`s6`), in list order: log lines stay under perf_log's 500 characters. */
const cards = () => listRows().filter((row) => row.type === "card").map((row) => row.type === "card" ? row.session.name.split("/").pop() ?? "" : "");
const TABS: StateFilter[] = ["all", "attention", "active", "paused"];

/** Every tab's count and listed cards; throws when a tab lists other than it counts. */
async function measure(log: (line: string) => Promise<unknown>, step: string) {
  const report: Record<string, { count: number; listed: string[] }> = {};
  for (const tab of TABS) {
    setStateFilter(tab);
    await sleep(250);
    const count = filterCounts()[tab as Exclude<StateFilter, "archived">];
    report[tab] = { count, listed: cards() };
    await log(`TAB ${step} ${tab} ` + JSON.stringify(report[tab]));
    if (count !== report[tab].listed.length) throw Error(`${step} ${tab}: count ${count} but ${report[tab].listed.length} listed`);
  }
  return report;
}

/** Only the explicit fake CLI and synthetic replay can run this interaction test. */
export async function attentionFilterSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "attention-filter-smoke: " + line });
  try {
    // The synthetic replay is the local computer ("") catalog: wait until it stops streaming, then the
    // fixtures replace its rows.
    await until(() => Object.keys(catalog.rows).some((key) => key.startsWith("\n")), "replay catalog missing");
    let size = -1, stable = 0;
    for (let tries = 0; tries < 60 && stable < 8; tries++) {
      const now = Object.keys(catalog.rows).length;
      stable = now === size ? stable + 1 : 0;
      size = now;
      await sleep(250);
    }
    setPage("sessions"); setSearch(""); setSelectedKey(undefined); setProjectFilter([]); setAccountFilter([]); setFolderFilter(undefined);
    applyBatch({ ...emptyBatch(), machines: [{ computer: "", machine: { machine_id: MACHINE } as MachineMeta }],
      replaces: [{ computer: "", rows: ATTENTION_FIXTURES.map((row, i) => ({ computer: "", key: `run:fixture-${i + 1}`, row })) }] });
    await until(() => cards().length === ATTENTION_FIXTURES.length, "fixture rows missing");
    for (const row of ATTENTION_FIXTURES) await log(`FIXTURE ${row.name.split("/").pop()} ${row.title}`);
    await measure(log, "before");
    setStateFilter("attention");
    await sleep(600);
    await log("READY " + JSON.stringify({ count: filterCounts().attention, listed: cards() }));
    // The driver screenshots the attention tab now; keep it still.
    await sleep(4000);
    await measure(log, "after");
    setStateFilter("attention");
    await sleep(300);
    await log("PASS");
  } catch (error) {
    await log("FAIL " + String(error));
  }
}
