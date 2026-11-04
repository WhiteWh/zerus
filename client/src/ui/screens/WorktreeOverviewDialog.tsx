import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { Icon } from "../lib/icons";
import { MachineLabel } from "../components/MachineLabel";
import { copy } from "../../store/actions";
import type { Project } from "../../types";
import { CleanupInspection, type InspectionRow } from "./WorktreeCleanup";
import { WorktreeReadLane } from "./worktree-read-lane";
import type { Inspection } from "../../store/terminationCleanup";
import { CATEGORIES, PAGE, LIMIT, createOverview, emptyFilter, noConsents, type Filter, type Overview, type OverviewRow } from "./worktree-overview-state";
import "../styles/worktree-overview.css";
import { tr } from "../../i18n";
const leaf = (p: string) => p.split(/[\\/]/u).filter(Boolean).at(-1) ?? p;
const number = (n: number) => n.toLocaleString();
const updated = (n: number) => new Date(n * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
export default function WorktreeOverviewDialog(props: { projects: Project[]; all: boolean; origin: HTMLElement; close(): void }) {
  let dialog!: HTMLDialogElement, tree!: HTMLDivElement, filters!: HTMLDivElement, filtersButton!: HTMLButtonElement;
  let closed = false, unsub: (() => void)[] = [], anchor: string | undefined;
  const state = createOverview();
  const [filter, setFilter] = createSignal<Filter>(emptyFilter());
  const [focused, setFocused] = createSignal<OverviewRow>();
  const [inspectionGeneration, setInspectionGeneration] = createSignal(0);
  const lane = new WorktreeReadLane<Inspection>();
  const view = state.frame;
  const rows = () => view()?.page.groups.flatMap((g) => g.rows) ?? [];
  const byId = createMemo(() => new Map(rows().map((r) => [r.id, r])));
  const groups = () => view()?.page.groups ?? [];
  const selected = (id: string) => view()?.page.selected_ids.includes(id) ?? false;
  const busy = () => state.working() || state.reading();
  const reading = () => busy() || !!view()?.reading;
  const totalPages = () => Math.max(1, Math.ceil((view()?.page.total ?? 0) / PAGE));
  const machines = createMemo(() => [...new Set(props.projects.flatMap((p) => p.folders.map((f) => f.machine)))].sort());
  const change = (next: Partial<Filter>) => setFilter({ ...filter(), ...next });
  createEffect(on(filter, (next) => { anchor = undefined; void state.query(next); }, { defer: true }));
  createEffect(() => { const v = view(); if (!v) return; const current = focused(); if (current) setFocused(rows().find((r) => r.id === current.id)); });
  const click = (event: MouseEvent, row: OverviewRow) => {
    if (busy()) return;
    setFocused(row);
    if (row.category === 4) { void state.select([], true); return; }
    const ids = rows().map((r) => r.id), a = anchor ? ids.indexOf(anchor) : -1, b = ids.indexOf(row.id);
    if (event.shiftKey && a >= 0) void state.select(rows().slice(Math.min(a, b), Math.max(a, b) + 1).filter((r) => r.category !== 4).map((r) => r.id), true);
    else void state.select([row.id], !(event.ctrlKey || event.metaKey));
    anchor = row.id;
  };
  const page = async (index: number, button: HTMLButtonElement) => {
    anchor = undefined;
    let moved = false;
    const mark = () => { moved = true; };
    button.focus();
    addEventListener("pointerdown", mark, true); addEventListener("keydown", mark, true);
    try {
      await state.page(index);
      if (moved || !button.isConnected) return;
      if (button.disabled) { tree.querySelector<HTMLElement>(".wto-row[data-row]")?.focus(); const row = rows()[0]; if (row) setFocused(row); }
      else button.focus();
    } finally { removeEventListener("pointerdown", mark, true); removeEventListener("keydown", mark, true); }
  };
  const refresh = async () => { await state.refresh(); setInspectionGeneration((n) => n + 1); };
  const showFilters = () => { const rect = filtersButton.getBoundingClientRect(); filters.style.left = `${rect.left}px`; filters.style.top = `${rect.bottom + 4}px`; filters.togglePopover(); };
  onMount(() => {
    dialog.showModal();
    void (async () => {
      const off = await Promise.all([listen<{ id: string }>("worktrees-overview-changed", (event) => state.changed(event.payload.id)), listen("worktree-manual-changed", () => state.batchChanged())]);
      if (closed) { off.forEach((fn) => fn()); return; } unsub = off;
      await state.open(props.all);
      if (!closed) dialog.querySelector<HTMLInputElement>("input[type=search]")?.focus();
    })();
  });
  onCleanup(() => { closed = true; unsub.forEach((fn) => fn()); lane.close(); state.dispose(); dialog.close(); queueMicrotask(() => { if (props.origin.isConnected) props.origin.focus(); }); });
  const inspection = (): InspectionRow | undefined => { const row = focused(); return row ? { id: row.id, path: row.path, label: leaf(row.path), machine: row.machine, computer: row.computer, branch: row.branch, reason: row.remains, details: `Folder: ${row.path}\nComputer: ${row.machine}\nBranch: ${row.branch || "—"}\nSize: ${row.size}\nSessions: ${row.sessions.join(", ") || "none"}` } : undefined; };
  return <>
    <dialog ref={dialog} class="dlg su-dlg worktree-overview" aria-labelledby="worktree-overview-title" style={{ width: "min(1180px, calc(100vw - 48px))", height: "min(760px, calc(100vh - 48px))", position: "fixed", inset: "0", margin: "auto" }} onCancel={(event) => { event.preventDefault(); props.close(); }}>
      <div class="dlg-head"><Icon name="folder-git-2" size={18} class="accent" /><h2 id="worktree-overview-title">{tr("Worktrees")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={props.close}><Icon name="x" size={16} /></button></div>
      <div class="dlg-body wto-body">
        <div class="row wto-toolbar"><input class="field grow" type="search" placeholder={tr("Search worktrees…")} aria-label={tr("Search folder, branch, project or session")} maxLength={4096} value={filter().search} onInput={(event) => change({ search: event.currentTarget.value })} />
          <button ref={filtersButton} class="btn" aria-haspopup="dialog" classList={{ on: !!(filter().project || filter().computer || filter().category !== null || filter().show_main) }} onClick={showFilters}><Icon name="list-filter" size={14} />{tr("Filters")}</button>
          <div ref={filters} class="su-popover wto-filters" popover="auto" role="dialog" aria-label={tr("Filters")}>
            <div class="form-row"><label for="wto-project">{tr("Project")}</label><select id="wto-project" class="field" value={filter().project} onChange={(e) => change({ project: e.currentTarget.value })}><option value="">{tr("All projects")}</option><For each={props.projects}>{(p) => <option value={p.id}>{p.name}</option>}</For></select></div>
            <div class="form-row"><label for="wto-computer">{tr("Computer")}</label><select id="wto-computer" class="field" value={filter().computer} onChange={(e) => change({ computer: e.currentTarget.value })}><option value="">{tr("All computers")}</option><For each={machines()}>{(m) => <option value={m}>{m}</option>}</For></select></div>
            <div class="form-row"><label for="wto-state">{tr("State")}</label><select id="wto-state" class="field" aria-label={tr("Worktree state")} value={filter().category ?? -1} onChange={(e) => change({ category: +e.currentTarget.value < 0 ? null : +e.currentTarget.value })}><option value="-1">{tr("All states")}</option><For each={CATEGORIES.slice(0, 4)}>{(name, i) => <option value={i()}>{tr(name)}</option>}</For></select></div>
            <label class="check"><input type="checkbox" checked={filter().show_main} onChange={(e) => change({ show_main: e.currentTarget.checked })} />{tr("Show main checkouts and ordinary folders")}</label>
          </div>
          <select class="field wto-sort" aria-label={tr("Sort worktrees")} value={filter().sort} onChange={(e) => change({ sort: e.currentTarget.value as Filter["sort"] })}><option value="size">{tr("Largest first")}</option><option value="activity">{tr("Oldest activity first")}</option><option value="name">{tr("Folder name")}</option></select>
          <button class="btn" disabled={reading()} onClick={() => void refresh()}><Icon name="refresh-cw" size={13} class={view()?.reading ? "su-spin" : undefined} />{tr("Refresh")}</button>
        </div>
        <div class="hint wto-status" title={tr("Browsing data is cached for 1 hour. Cleanup checks expire after five minutes.")}><Show when={view()}>{(v) => <span class="parts"><span>{tr("Processed %1 of %2 folders", number(v().processed), number(v().total))}</span><span>{tr("%1 repositories", number(v().repositories))}</span><Show when={v().incomplete}><span>{tr("%1 incomplete", number(v().incomplete))}</span></Show><Show when={v().pending}><span>{tr("%1 pending…", number(v().pending))}</span></Show><span>{tr("%1 cleanup candidates", number(v().page.candidates))}</span><Show when={v().page.estimated_bytes !== null}><span>{tr("~%1 GB estimated", ((v().page.estimated_bytes ?? 0) / 1e9).toFixed(1))}</span></Show><Show when={v().partial || v().page.partial}><span class="warn">{tr("Catalog is incomplete; some folders are not shown.")}</span></Show><Show when={v().updated}>{(n) => <span>{tr("Updated %1", updated(n()))}</span>}</Show></span>}</Show></div>
        <div class="wto-split"><div class="wto-left"><div ref={tree} class="wto-tree" role="grid" aria-label={tr("Worktrees")} aria-multiselectable="true">
          <div class="wto-row wto-headrow" role="row"><span role="columnheader" /><span role="columnheader">{tr("Worktree / branch")}</span><span role="columnheader">{tr("What remains")}</span><span role="columnheader">{tr("Size")}</span><span role="columnheader">{tr("Computer")}</span></div>
          <Show when={view()?.total === 0 && !view()?.reading}><div class="empty"><Icon name="folder-git-2" size={22} /><p>{tr("Add a project folder to see its worktrees.")}</p></div></Show>
          <For each={groups().map((g) => g.category)}>{(category) => { const group = () => groups().find((g) => g.category === category)!; return <><div class="wto-group" role="row">{group().name} ({number(group().total)})</div><For each={group().rows.map((r) => r.id)}>{(id) => { const row = () => byId().get(id)!; return <div class="wto-row" data-row={row().id} tabindex="0" role="row" aria-selected={selected(row().id)} classList={{ on: selected(row().id), focus: focused()?.id === row().id, stale: !row().fresh }} onClick={(e) => click(e, row())}>
              <span role="gridcell"><input type="checkbox" aria-label={`Select ${leaf(row().path)}`} checked={selected(row().id)} disabled={row().category === 4 || busy()} onClick={(e) => { e.preventDefault(); e.stopPropagation(); if (!busy()) { setFocused(row()); anchor = row().id; void state.select([row().id], false); } }} /></span>
              <span role="gridcell" class="col"><span class="ell"><b>{leaf(row().path)}</b></span><span class="mono dim ell">{row().branch || "—"}</span></span>
              <span role="gridcell" class="ell" classList={{ warn: row().category === 1 || row().category === 3, muted: row().category === 4 }} title={row().remains}><Show when={row().state && row().state !== "Ready" && row().state !== row().remains}><span class="wto-state">{row().state}</span> </Show>{row().remains}<Show when={row().category === 0 && row().fresh}><span class="wto-suffix">{tr("Will recheck before cleanup")}</span></Show><Show when={!row().fresh && row().state !== "Result unknown"}><span class="wto-suffix">{tr("Offline snapshot")}</span></Show></span>
              <span role="gridcell" class="tabular">{row().size}</span><span role="gridcell"><MachineLabel alias={row().machine} class="mbadge" /></span>
            </div>; }}</For></>; }}</For>
        </div><Show when={(view()?.page.total ?? 0) > PAGE}><div class="row wto-pager"><span class="muted tabular">{tr("%1–%2 of %3 worktrees", number((view()?.page.index ?? 0) * PAGE + 1), number(Math.min(((view()?.page.index ?? 0) + 1) * PAGE, view()?.page.total ?? 0)), number(view()?.page.total ?? 0))}</span><span class="grow" /><button class="btn small" aria-label={tr("Previous page of worktrees")} disabled={busy() || !view()?.page.index} onClick={(e) => void page((view()?.page.index ?? 0) - 1, e.currentTarget)}><Icon name="chevron-left" size={14} />{tr("Previous")}</button><button class="btn small" aria-label={tr("Next page of worktrees")} disabled={busy() || (view()?.page.index ?? 0) + 1 >= totalPages()} onClick={(e) => void page((view()?.page.index ?? 0) + 1, e.currentTarget)}>{tr("Next")}<Icon name="chevron-right" size={14} /></button></div></Show></div>
          <Show when={focused()?.id ? `${focused()!.id}:${inspectionGeneration()}` : undefined} keyed fallback={<section class="su-insp"><div class="empty"><Icon name="folder-git-2" size={22} /><p>{tr("Select a worktree")}</p></div></section>}>{(_key) => <CleanupInspection row={inspection()!} panel enabled={!view()?.reading} close={props.close} read={(id, file = "", offset = 0) => lane.read(() => state.inspect(id, file, offset))} />}</Show>
        </div>
      </div>
      <div class="dlg-foot"><button class="btn" disabled={!focused()} onClick={() => void copy(focused()!.path)}><Icon name="copy" size={13} />{tr("Copy path")}</button><button class="btn" disabled={reading() || !view()?.page.candidates || (view()?.page.candidates ?? 0) > LIMIT} title={(view()?.page.candidates ?? 0) > LIMIT ? tr("More than 64 cleanup candidates. Filter by project or computer first.") : undefined} onClick={() => void state.candidates()}>{tr("Select candidates")}</button><span class="grow" /><button class="btn primary" disabled={reading() || !view()?.page.selected || (view()?.page.selected ?? 0) > LIMIT} title={(view()?.page.selected ?? 0) > LIMIT ? tr("Select at most 64 worktrees for one cleanup.") : undefined} onClick={() => void state.review()}>{view()?.page.selected ? tr("Review cleanup (%1)…", number(view()!.page.selected)) : tr("Review cleanup…")}</button><button class="btn" onClick={props.close}>{tr("Close")}</button></div>
    </dialog>
    <Show when={state.batch()?.id} keyed>{(_key) => <ManualDialog state={state} close={() => { void state.closeBatch().then(() => setInspectionGeneration((n) => n + 1)); }} />}</Show>
  </>;
}
function ManualDialog(props: { state: Overview; close(): void }) {
  let dialog!: HTMLDialogElement;
  const state = props.state, view = state.batch;
  const key = view()!.id, origin = document.activeElement;
  const [selected, setSelected] = createSignal(view()!.rows[0]?.id), [details, setDetails] = createSignal(false), [consents, setConsents] = createSignal(noConsents()), [inspect, setInspect] = createSignal<string>();
  const current = () => view()?.rows.find((r) => r.id === selected()) ?? view()?.rows[0];
  const count = () => view()?.rows.filter((r) => r.ready).length ?? 0;
  const busy = () => !!view()?.busy || state.working();
  const consented = () => { const required = view()?.required; return required && (!required.local_files || consents().local_files) && (!required.discard_changes || consents().discard_changes) && (!required.unchecked_processes || consents().unchecked_processes); };
  const lane = new WorktreeReadLane<Inspection>();
  const inspected = () => view()?.rows.find((r) => r.id === inspect());
  createEffect(() => { const value = view(); if (value && !value.busy) setConsents(noConsents()); });
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLButtonElement>("[autofocus]")?.focus()); });
  onCleanup(() => { lane.close(); dialog.close(); if (origin instanceof HTMLElement && origin.isConnected) origin.focus(); });
  const close = () => { if (!busy()) props.close(); };
  return <>
    <dialog ref={dialog} class="dlg su-dlg worktree-cleanup worktree-manual" style={{ width: "780px", position: "fixed", inset: "0", margin: "auto" }} aria-labelledby="worktree-manual-title" onCancel={(e) => { e.preventDefault(); close(); }}>
      <div class="dlg-head"><Icon name="trash" size={18} class="accent" /><h2 id="worktree-manual-title">{tr("Remove working folders")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} disabled={busy()} onClick={close}><Icon name="x" size={16} /></button></div>
      <div class="dlg-body bc-body"><h3 class="section-title">{view()?.rows.length === 1 ? tr("Review 1 selected worktree") : tr("Review %1 selected worktrees", view()?.rows.length ?? 0)}</h3><p class="muted">{tr("Conversations are kept. Branches stay unless you choose to delete them. Local files in removed folders are permanently deleted.")}</p>
        <table class="su-table bc-table"><thead><tr><th>{tr("Folder / branch")}</th><th>{tr("Computer")}</th><th>{tr("Size")}</th><th>{tr("Result")}</th></tr></thead><tbody><For each={view()?.rows}>{(row) => <tr classList={{ on: current()?.id === row.id }} title={row.path} onClick={() => setSelected(row.id)}><td><div class="ell"><b>{row.label}</b></div><div class="mono dim ell">{row.branch || "—"}</div></td><td><MachineLabel alias={row.machine} class="mbadge" /></td><td class="tabular">{row.size}</td><td title={row.reason}><div class="bc-result" data-outcome={row.status}><Show when={row.status === "checking" || row.status === "removing"}><Icon name="loader-circle" size={12} class="su-spin" /></Show><b>{row.status === "removed" ? row.forgotten ? tr("Record forgotten") : tr("Folder removed") : row.status === "unknown" ? tr("Result unknown") : row.status === "kept" ? tr("Folder kept") : row.status === "checking" ? tr("Checking…") : row.status === "removing" ? tr("Checking again and removing…") : "Ready"}</b><Show when={!["removed", "checking", "removing"].includes(row.status)}><span class="hint ell wtc-reason">{row.reason}</span></Show></div></td></tr>}</For></tbody></table>
        <div class="row"><button class="btn small" disabled={busy() || !current()?.can_check} onClick={() => void state.check()}>{tr("Check again")}</button><button class="btn small" disabled={busy() || !current()} onClick={() => setInspect(current()?.id)}>{tr("View files and commits…")}</button><button class="btn small" classList={{ on: details() }} aria-pressed={details()} onClick={() => setDetails(!details())}>{tr("Details")}</button><span class="grow" /></div><Show when={details()}><pre class="bc-details mono">{current()?.details}</pre></Show>
        <div class="bc-consent"><Show when={count() && view()?.required.local_files}><label class="check"><input type="checkbox" checked={consents().local_files} disabled={busy()} onChange={(e) => setConsents({ ...consents(), local_files: e.currentTarget.checked })} />{tr("Delete the local files outside Git too")}</label></Show><Show when={count() && view()?.required.discard_changes}><label class="check"><input type="checkbox" checked={consents().discard_changes} disabled={busy()} onChange={(e) => setConsents({ ...consents(), discard_changes: e.currentTarget.checked })} />{tr("Permanently discard uncommitted and untracked files")}</label></Show><Show when={count() && view()?.required.unchecked_processes}><label class="check" title={tr("Accept this uncertainty for this folder for five minutes. Known programs and sessions using the folder still prevent removal.")}><input type="checkbox" checked={consents().unchecked_processes} disabled={busy()} onChange={(e) => setConsents({ ...consents(), unchecked_processes: e.currentTarget.checked })} />{tr("Remove even if some running programs cannot be checked")}</label></Show><label class="check"><input type="checkbox" checked={consents().merged_branches} disabled={busy()} onChange={(e) => setConsents({ ...consents(), merged_branches: e.currentTarget.checked })} />{tr("Also delete branches proven merged by Git")}</label></div>
        <Show when={view()?.cleaning}><progress aria-label={tr("Removing working folders")} /></Show>
      </div>
      <div class="dlg-foot"><span class="bc-summary tabular" role="status">{view()?.stopping ? tr("Finishing the current folder; remaining folders will be kept.") : view()?.attempted ? tr("%1 removed  %2 kept  %3 unknown", view()?.removed ?? 0, view()?.kept ?? 0, view()?.unknown ?? 0) : view()?.busy ? tr("Checking folders…") : ""}<Show when={view()?.unknown}><span class="warn bc-note">{tr("An unknown result will not be retried.")}</span></Show></span><span class="grow" /><Show when={!view()?.attempted}><button type="button" class="btn solid-danger" disabled={busy() || !count() || !consented()} onClick={() => void state.remove(consents())}>{count() > 1 ? `Remove ${count()} folders` : tr("Remove folders")}</button></Show><button type="button" class="btn primary" autofocus disabled={state.working() || view()?.stopping || !!view()?.busy && !view()?.cleaning} onClick={() => { if (view()?.cleaning) void state.stop(); else close(); }}>{view()?.cleaning ? tr("Stop after this folder") : view()?.attempted ? "Close" : tr("Keep folders")}</button></div>
    </dialog>
    <Show when={inspected()}>{(row) => <CleanupInspection row={row()} close={() => setInspect(undefined)} read={(id, file = "", offset = 0) => lane.read(() => state.inspectBatch(key, id, file, offset))} />}</Show>
  </>;
}
