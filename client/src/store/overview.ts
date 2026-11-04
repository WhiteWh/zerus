import { createMemo, type Accessor } from "solid-js";
import type { ComputerState } from "./catalog";
import { attentionNeeded, isActive, needsAttention } from "./attention";
import type { ComputerId, MachineMeta } from "../types";
import type { Session } from "../ui/sessions/types";
import type { MachineState } from "../ui/components/MachineTile";
import type { OsKind } from "../ui/lib/os-icons";
import { tr } from "../i18n";

// The Overview's selectors (DashboardPage::render, DashboardPage.cpp:401-456).
// The Overview screen creates them, so they cost nothing on other pages; each
// is one pass over the catalog per batch and none runs on a timer.

/** A machine's connection as the Overview and Machines show it (Decisions note). */
export interface Connection { tile: MachineState; text: string; fresh: boolean }

/** Only a live stream is fresh: the "Machines online" tile, the load colours and the offline notice count it. */
export function connectionOf(status: string | undefined): Connection {
  switch (status) {
    case "live": return { tile: "online", text: "Connected", fresh: true };
    case "polled": return { tile: "online", text: tr("Update hgs"), fresh: true };
    case "probing": return { tile: "connecting", text: "Connecting", fresh: false };
    case "reconnecting": return { tile: "connecting", text: "Reconnecting", fresh: false };
    case "unsupported": return { tile: "offline", text: tr("Update hgs"), fresh: false };
    default: return { tile: "offline", text: "Offline", fresh: false };
  }
}

export const osOf = (platform: string | null | undefined): OsKind | undefined =>
  platform === "linux" || platform === "macos" || platform === "windows" ? platform : undefined;

export interface MachineCardModel {
  computer: ComputerId;
  /** The badge: this computer's host name, a peer's alias. */
  name: string;
  /** The Sessions machine filter's value for this computer's rows. */
  filter: string;
  local: boolean;
  platform?: OsKind;
  connection: Connection;
  meta?: MachineMeta;
}

/** This computer unless it is a control console (DashboardPage.cpp:407-409), then every peer that has not ended. */
export function fleetCards(computers: Record<ComputerId, ComputerState>, cards: Record<ComputerId, MachineMeta>): MachineCardModel[] {
  const out: MachineCardModel[] = [];
  const localMeta = cards[""];
  if ((computers[""] || localMeta) && localMeta?.local_sessions !== false) {
    out.push({
      computer: "", name: localMeta?.host || tr("This machine"), filter: localMeta?.host || "this computer", local: true,
      platform: osOf(localMeta?.platform), connection: connectionOf(computers[""]?.status), meta: localMeta,
    });
  }
  for (const id of Object.keys(computers).filter((id) => id !== "" && computers[id].status !== "ended").sort()) {
    out.push({ computer: id, name: id, filter: id, local: false, platform: osOf(cards[id]?.platform), connection: connectionOf(computers[id].status), meta: cards[id] });
  }
  return out;
}

export interface OverviewStats {
  all: number;
  active: number;
  attention: number;
  /** Rows on machines whose stream is not live (the offline notice). */
  offline: number;
  per: Record<ComputerId, { sessions: number; attention: number }>;
}

/** A computer's rows are current: its stream is live or polled (Qt's `machine.fresh`). */
const fresh = (computers: Record<ComputerId, ComputerState>, computer: ComputerId) =>
  connectionOf(computers[computer]?.status).fresh;

/**
 * The total follows the window's rule on current machines
 * (`SessionPresentation::needsAttention(session, machine.fresh)`,
 * DashboardPage.cpp:426-427); a machine card counts Qt's
 * `session.needsAttention()` (DashboardPage.cpp:133), without subagents
 * and whatever its freshness.
 */
export function overviewStats(rows: readonly Session[], computers: Record<ComputerId, ComputerState>): OverviewStats {
  const stats: OverviewStats = { all: 0, active: 0, attention: 0, offline: 0, per: {} };
  for (const s of rows) {
    const per = (stats.per[s.computer] ??= { sessions: 0, attention: 0 });
    const current = fresh(computers, s.computer);
    stats.all++;
    per.sessions++;
    if (!s.muted && needsAttention(s, current)) stats.attention++;
    if (!s.muted && attentionNeeded(s)) per.attention++;
    if (isActive(s, current)) stats.active++;
    if (!current) stats.offline++;
  }
  return stats;
}

/**
 * Working, attention and draft sessions of current machines' activity:
 * `session.needsAttention()` first (DashboardPage.cpp:453), then the latest
 * event, then the key (DashboardPage.cpp:452-456).
 */
export function liveSessions(rows: readonly Session[], computers: Record<ComputerId, ComputerState>): Session[] {
  return rows.filter((s) => isActive(s, fresh(computers, s.computer))).sort((a, b) =>
    Number(attentionNeeded(b)) - Number(attentionNeeded(a))
    || (b.updatedAt ?? 0) - (a.updatedAt ?? 0)
    || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
}

export function createOverview(
  rows: Accessor<readonly Session[]>,
  computers: Accessor<Record<ComputerId, ComputerState>>,
  cards: Accessor<Record<ComputerId, MachineMeta>>,
  compute: typeof overviewStats = overviewStats,
) {
  return {
    stats: createMemo(() => compute(rows(), computers())),
    live: createMemo(() => liveSessions(rows(), computers())),
    cards: createMemo(() => fleetCards(computers(), cards())),
  };
}
