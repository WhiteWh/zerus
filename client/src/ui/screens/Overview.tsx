import { For, Index, Show, createSignal, onCleanup, onMount } from "solid-js";
import { useNow } from "../../store/latest";
import { createVirtualizer } from "@tanstack/solid-virtual";
import { Icon } from "../lib/icons";
import { MachineTile } from "../components/MachineTile";
import { UsageMeters } from "../components/UsageMeter";
import { AgentBadge, MachineBadge, SessionCard } from "../sessions/SessionCard";
import type { Session } from "../sessions/types";
import type { Account } from "../../types";
import { agentKind, catalog, orderedRows } from "../../store/catalog";
import { machineCards, requestMachine } from "../../store/machines";
import { accountsPage, loadAccounts, setAccountMachine, setRequestedProfile } from "../../store/accounts";
import { createOverview, type MachineCardModel, type OverviewStats } from "../../store/overview";
import { setMachineFilter, setPage, setProjectFilter, setAccountFilter, setFolderFilter, setSelectedKey, setStateFilter, type StateFilter } from "../../store/ui";
import { flag, num, setSetting } from "../../store/settings";
import { accountTooltip, clock, summaryText, usageLoading, windowsAt } from "../lib/usage";
import { machineKey } from "./machines-ui";
import { accountIdentity, accountTone, accountsNote, cpuFigure, level, loadTip, memoryFigure, sampleNote, sampleTip, sessionsLabel } from "./overview-ui";
import { tr } from "../../i18n";

// Overview (DashboardPage.cpp), ported from design/webui/src/screens/Dashboard.tsx:
// summary tiles, a card per machine from the stream's machine cards, account
// cards from the screens hub, then the sessions that are working, need
// attention or have drafts. A sleeping peer is a pause, not a death: its last
// known sessions keep counting and its card says its stream is not live.

const CARD_W = 340;
const CARD_H = 104;
const GAP = 8;

function LoadBar(props: { percent?: number; fresh: boolean }) {
  return (
    <div class="ov-load" data-level={level(props.percent, props.fresh)} title={loadTip(props.percent, props.fresh)}>
      <i style={{ width: `${props.percent === undefined ? 0 : Math.max(0, Math.min(100, props.percent))}%` }} />
    </div>
  );
}

function MachineCard(props: { m: MachineCardModel; stats: OverviewStats; now: number }) {
  const c = () => props.m.connection;
  const cpu = () => cpuFigure(props.m.meta);
  const memory = () => memoryFigure(props.m.meta);
  const counts = () => props.stats.per[props.m.computer] ?? { sessions: 0, attention: 0 };
  const note = () => sampleNote(props.m.meta, c(), props.now);
  const openSessions = () => { setMachineFilter([props.m.filter]); setProjectFilter([]); setAccountFilter([]); setFolderFilter(undefined); setStateFilter("all"); setPage("sessions"); };
  return (
    <article class="ov-mcard box" classList={{ stale: !c().fresh }} aria-label={props.m.local ? tr("This machine") : props.m.name}>
      <header class="ov-mtop">
        <MachineTile os={props.m.platform} state={c().tile} size={26} />
        <MachineBadge alias={props.m.name} computer={props.m.computer} />
        <span class="ov-mos ell"><Show when={props.m.local}><b>{tr("This machine")}</b></Show></span>
        <span class="ov-mstatus" classList={{ online: c().fresh }}>{c().text}</span>
        <button class="icon-btn small" data-tip={tr("Machine settings")} aria-label={tr("Machine settings")} onClick={() => { requestMachine(machineKey(props.m.computer)); setPage("machines"); }}><Icon name="settings" size={14} /></button>
      </header>
      <div class="ov-mmeters">
        <div class="ov-mm" title={cpu().tip}>
          <span class="ov-mlabel">{tr("CPU")}</span>
          <span class="ov-mline">
            <b class="ov-mval tabular" data-level={level(cpu().percent, c().fresh)}>{cpu().value}</b>
            <LoadBar percent={cpu().percent} fresh={c().fresh} />
          </span>
          <span class="ov-mdetail">{cpu().detail}</span>
        </div>
        <div class="ov-mm" title={memory().tip}>
          <span class="ov-mlabel">{tr("Memory")}</span>
          <span class="ov-mline">
            <b class="ov-mval tabular" data-level={level(memory().percent, c().fresh)}>{memory().value}</b>
            <LoadBar percent={memory().percent} fresh={c().fresh} />
          </span>
          <span class="ov-mdetail">{memory().detail}</span>
        </div>
      </div>
      <footer class="ov-mfoot">
        <button class="su-link" title={c().fresh ? tr("View sessions on %1", props.m.name) : tr("View last known sessions on %1", props.m.name)} onClick={openSessions}>
          {sessionsLabel(counts().sessions, counts().attention)}
        </button>
        <span class="grow" />
        <Show when={note()}><span class="ov-mdetail" title={sampleTip(c())}>{note()}</span></Show>
      </footer>
    </article>
  );
}

function AccountCard(props: { a: Account; now: number }) {
  // An agent that is not installed reads as unavailable (DashboardPage.cpp:262).
  const usage = () => (props.a.installed ? props.a.usage : { ...props.a.usage, status: "unavailable" });
  const hosts = () => props.a.members.filter((m, i, all) => all.findIndex((x) => x.host === m.host) === i);
  const open = () => {
    setRequestedProfile(props.a.members[0]?.key);
    setAccountMachine("*");
    setPage("accounts");
  };
  return (
    <button class="ov-acard box" data-tone={accountTone(props.a, props.now)} title={accountTooltip(usage(), props.now)} onClick={open}>
      <span class="ov-al1">
        <b class="ell">{props.a.label}</b>
        <span class="grow" />
        <AgentBadge agent={agentKind(props.a.provider)} />
        <For each={hosts()}>{(m) => <MachineBadge alias={m.machine} computer={m.host} />}</For>
      </span>
      <span class="ov-al2">
        <span class="ell muted">{accountIdentity(props.a)}</span>
        <span class="grow" />
        <span class="ov-ausage tabular">
          <UsageMeters windows={windowsAt(usage().windows, props.now)} loading={usageLoading(usage())} compact summary={summaryText(usage())} />
          <Show when={usage().seeded && usage().checked_at !== null}><span class="dim">Checked {clock(usage().checked_at!)}</span></Show>
        </span>
      </span>
    </button>
  );
}

function LiveSessions(props: { rows: Session[]; scroller: () => HTMLElement | undefined }) {
  let grid!: HTMLDivElement;
  const [width, setWidth] = createSignal(1000);
  const [top, setTop] = createSignal(0);
  const cols = () => Math.max(1, Math.floor((width() + GAP) / (CARD_W + GAP)));
  const lines = () => Math.ceil(props.rows.length / cols());
  // The page scroller is attached only after this grid mounts; reading it
  // earlier would measure a detached element as 0 px high.
  let attached = false;
  const virtualizer = createVirtualizer({
    get count() { return lines(); },
    getScrollElement: () => (attached ? props.scroller() ?? null : null),
    estimateSize: () => CARD_H + GAP,
    overscan: 4,
    get scrollMargin() { return top(); },
  });
  attached = true;
  onMount(() => {
    const measure = () => { setWidth(grid.clientWidth); setTop(grid.offsetTop); };
    const ro = new ResizeObserver(measure);
    ro.observe(grid);
    // Content above the grid (accounts loading, machine cards, the offline notice) moves it without resizing it.
    if (grid.parentElement) ro.observe(grid.parentElement);
    measure();
    onCleanup(() => ro.disconnect());
  });
  const open = (s: Session) => { setSelectedKey(s.key); setPage("sessions"); };
  return (
    <div class="ov-live" ref={grid} style={{ height: `${virtualizer.getTotalSize()}px` }}>
      <For each={virtualizer.getVirtualItems()}>
        {(line) => (
          <div class="ov-line" style={{ transform: `translateY(${line.start - top()}px)`, "grid-template-columns": `repeat(${cols()}, minmax(0, 1fr))` }}>
            <For each={props.rows.slice(line.index * cols(), line.index * cols() + cols())}>
              {(s) => (
                <div class="ov-cell rowhit" role="button" tabindex="0" aria-label={tr("Open %1", s.name)} onClick={() => open(s)}
                  onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); open(s); } }}>
                  <SessionCard session={s} selected={false} expandable={false} expanded={false} onToggleChildren={() => open(s)} />
                </div>
              )}
            </For>
          </div>
        )}
      </For>
    </div>
  );
}

// A centred column like Activity: both edges drag together, double-click
// returns to the default. The width is Settings > Appearance > Overview width
// (client/overviewWidth, client/overviewFullWidth); a drag shows it live and
// stores it once, when the pointer lets go.
const OVERVIEW = { min: 720, max: 2400, edge: 12, fallback: 1200 } as const;
const clampWidth = (width: number) => Math.round(Math.max(OVERVIEW.min, Math.min(OVERVIEW.max, width)));
const [dragWidth, setDragWidth] = createSignal<number | undefined>();
const overviewWidth = () => dragWidth() ?? clampWidth(num("client/overviewWidth"));
const overviewFullWidth = () => flag("client/overviewFullWidth");
/** Stores a width once, and only when it differs from the stored one. */
function storeOverviewWidth(width: number) {
  setDragWidth(undefined);
  const next = clampWidth(width);
  if (next !== clampWidth(num("client/overviewWidth"))) void setSetting("client/overviewWidth", next);
}

function OverviewEdge(props: { right?: boolean }) {
  const [dragging, setDragging] = createSignal(false);
  // A drag cut off (capture lost, the page switched) leaves the stored width.
  const endDrag = () => { if (dragging()) { setDragging(false); setDragWidth(undefined); } };
  onCleanup(endDrag);
  let origin = 0, start = 0, widest = 0;
  return (
    <div class="col-edge ov-edge" classList={{ right: props.right, dragging: dragging() }} role="separator" aria-orientation="vertical" tabindex="0"
      aria-label={tr("Overview width")} aria-valuemin={OVERVIEW.min} aria-valuemax={OVERVIEW.max} aria-valuenow={overviewWidth()}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        const el = e.currentTarget;
        el.setPointerCapture(e.pointerId);
        origin = e.clientX;
        start = el.parentElement!.getBoundingClientRect().width;
        widest = (el.closest(".ov-page") as HTMLElement).clientWidth - 2 * OVERVIEW.edge;
        setDragging(true);
      }}
      onPointerMove={(e) => {
        if (!dragging()) return;
        const distance = (e.clientX - origin) * (props.right ? 1 : -1);
        setDragWidth(clampWidth(Math.min(widest, start + 2 * distance)));
      }}
      onPointerUp={() => {
        if (!dragging()) return;
        setDragging(false);
        // A click without movement stores nothing.
        const moved = dragWidth();
        if (moved !== undefined) storeOverviewWidth(moved);
      }}
      onPointerCancel={endDrag}
      onLostPointerCapture={endDrag}
      onDblClick={() => storeOverviewWidth(OVERVIEW.fallback)}
      onKeyDown={(e) => {
        if (e.key === "ArrowLeft" || e.key === "ArrowRight") { e.preventDefault(); storeOverviewWidth(overviewWidth() + ((e.key === "ArrowRight") === !!props.right ? 40 : -40)); }
        else if (e.key === "Home") { e.preventDefault(); storeOverviewWidth(OVERVIEW.fallback); }
      }} />
  );
}

export function OverviewScreen() {
  let scroller: HTMLDivElement | undefined;
  const ov = createOverview(orderedRows, () => catalog.computers, () => machineCards);
  // Ages, resets and pace re-render every 15 s while shown (DashboardPage.cpp:322); nothing else runs on a timer.
  const now = useNow(15_000);
  const view = accountsPage;
  const online = () => ov.cards().filter((m) => m.connection.fresh).length;
  const go = (filter: StateFilter) => { setMachineFilter([]); setProjectFilter([]); setAccountFilter([]); setFolderFilter(undefined); setStateFilter(filter); setPage("sessions"); };
  return (
    <div class="su-page ov-page" ref={scroller}>
      <div class="ov-content" classList={{ full: overviewFullWidth() }} style={{ "--ovw": `${overviewWidth()}px` }}>
        <Show when={!overviewFullWidth()}><OverviewEdge /><OverviewEdge right /></Show>
        <header class="su-head">
          <h1>{tr("Overview")}</h1>
          <span class="grow" />
          <button class="icon-btn" data-tip={tr("Refresh overview")} aria-label={tr("Refresh overview")} disabled={view().busy} onClick={() => void loadAccounts(true)}>
            <Icon name="refresh-cw" size={16} class={view().busy ? "su-spin" : undefined} />
          </button>
        </header>

        <div class="ov-tiles">
          <button class="ov-tile" classList={{ hot: ov.stats().attention > 0 }} onClick={() => go("attention")}>
            <b class="tabular">{ov.stats().attention}</b><span>{tr("Needs attention")}</span>
          </button>
          <button class="ov-tile" onClick={() => go("active")}><b class="tabular">{ov.stats().active}</b><span>{tr("Active")}</span></button>
          <button class="ov-tile" onClick={() => { setMachineFilter([]); setProjectFilter([]); setPage("machines"); }}><b class="tabular">{`${online()} / ${ov.cards().length}`}</b><span>{tr("Machines online")}</span></button>
          <button class="ov-tile" onClick={() => go("all")}><b class="tabular">{ov.stats().all}</b><span>{tr("Sessions")}</span></button>
        </div>

        <h2 class="section-title ov-h">{tr("Machines")}</h2>
        <div class="ov-mgrid">
          <Index each={ov.cards()}>{(m) => <MachineCard m={m()} stats={ov.stats()} now={now()} />}</Index>
        </div>

        <div class="ov-h row">
          <h2 class="section-title">{tr("Accounts")}</h2>
          <span class="grow" />
          <button class="su-link" onClick={() => { setAccountMachine("*"); setPage("accounts"); }}>{tr("Manage accounts")} <Icon name="arrow-right" size={12} /></button>
        </div>
        <Show when={accountsNote(view())} fallback={<div class="ov-agrid"><Index each={view().overview}>{(a) => <AccountCard a={a()} now={now()} />}</Index></div>}>
          {(note) => <p class="ov-empty">{note()}</p>}
        </Show>

        <div class="ov-h row">
          <h2 class="section-title">{tr("Attention & in progress")}</h2>
          <span class="grow" />
          <button class="su-link" onClick={() => go("all")}>{tr("All sessions")} <Icon name="arrow-right" size={12} /></button>
        </div>
        <Show when={ov.stats().offline > 0}>
          <div class="notice warn ov-note"><Icon name="wifi-off" size={14} />
            <span>{ov.stats().offline === 1 ? tr("1 session is on an offline or stale machine. Its last known status is shown on the machine cards.") : tr("%1 sessions are on offline or stale machines. Their last known status is shown on the machine cards.", ov.stats().offline)}</span>
          </div>
        </Show>
        <Show when={ov.live().length} fallback={<p class="ov-empty">{tr("No sessions are working, need attention or have drafts.")}</p>}>
          <LiveSessions rows={ov.live()} scroller={() => scroller} />
        </Show>
      </div>
    </div>
  );
}
