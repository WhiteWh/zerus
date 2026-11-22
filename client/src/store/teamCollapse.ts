import { createMemo } from "solid-js";
import { createStore } from "solid-js/store";
import { catalog } from "./catalog";
import { machineCards } from "./machines";
import { setSetting, settings, settingsReady } from "./settings";

// A parent's show/hide-children choice survives a relaunch (zerus-sa9qn.15,
// owner 2026-11-07). The device keeps the collapsed parents by their
// computer's machine UUID and durable session ID, never by name or run, so a
// rename, resume or restart keeps the choice. Explicit expansion is recorded
// separately so a retained favorite's ancestor can open its own branch.
// A row without those IDs keeps its choice for this window only.

export const COLLAPSED_TEAMS = "sessions/collapsedTeams";
export const EXPANDED_TEAMS = "sessions/expandedTeams";
export const MAX_COLLAPSED_TEAMS = 2048;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/u;

/** The durable ID of a parent: machine UUID and session ID, or undefined. */
export function teamId(machine: string | undefined, sessionId: string | undefined): string | undefined {
  return machine && sessionId && UUID.test(machine) && UUID.test(sessionId) ? `${machine}\n${sessionId}` : undefined;
}

/** The stored collapsed parents, oldest first; malformed entries are dropped (pure). */
export function parseTeams(raw: unknown): string[] {
  if (typeof raw !== "string" || !raw) return [];
  try {
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value)) return [];
    const ids = value.filter((id): id is string => typeof id === "string" && id.includes("\n")
      && teamId(...(id.split("\n") as [string, string])) === id);
    return [...new Set(ids)].slice(-MAX_COLLAPSED_TEAMS);
  } catch { return []; }
}

/** The list after one choice: the newest last, the oldest dropped first (pure). */
export function withTeam(list: readonly string[], id: string, collapsed: boolean): string[] {
  const next = list.filter((known) => known !== id);
  if (collapsed) next.push(id);
  return next.slice(-MAX_COLLAPSED_TEAMS);
}

const stored = createMemo(() => new Set(parseTeams(settings[COLLAPSED_TEAMS])));
const storedExpanded = createMemo(() => new Set(parseTeams(settings[EXPANDED_TEAMS])));
// This window's choices win over the stored ones, by row key.
const [overrides, setOverride] = createStore<Record<string, boolean>>({});

function durable(rowKey: string): string | undefined {
  const row = catalog.rows[rowKey];
  return row ? teamId(machineCards[row.computer]?.machine_id, row.sessionId) : undefined;
}

/** Whether a parent row's children are hidden. */
export function teamCollapsed(rowKey: string): boolean {
  const chosen = overrides[rowKey];
  if (chosen !== undefined) return chosen;
  const id = durable(rowKey);
  return !!id && stored().has(id);
}

/** An explicit own expansion, distinct from the default below a collapsed ancestor. */
export function teamExpanded(rowKey: string): boolean {
  const chosen = overrides[rowKey];
  if (chosen !== undefined) return !chosen;
  const id = durable(rowKey);
  return !!id && !stored().has(id) && storedExpanded().has(id);
}

// Choices not yet saved, newest last. Saving waits for the stored values: a
// choice made while they load joins the saved list instead of replacing it
// (zerus-xmjgp), as row density does.
const pending = new Map<string, boolean>();

/** The saved list with the pending choices applied, oldest first (pure). */
export function mergeTeams(saved: readonly string[], choices: Iterable<[string, boolean]>): string[] {
  let list = [...saved];
  for (const [id, collapsed] of choices) list = withTeam(list, id, collapsed);
  return list;
}

function save() {
  void settingsReady.then(() => {
    if (!pending.size) return;
    const choices = [...pending];
    const list = parseTeams(settings[COLLAPSED_TEAMS]);
    const next = mergeTeams(list, choices);
    const expanded = parseTeams(settings[EXPANDED_TEAMS]);
    const nextExpanded = mergeTeams(expanded, choices.map(([id, collapsed]) => [id, !collapsed]));
    pending.clear();
    if (next.length !== list.length || next.some((known, at) => known !== list[at]))
      void setSetting(COLLAPSED_TEAMS, JSON.stringify(next)).catch(() => {});
    if (nextExpanded.length !== expanded.length || nextExpanded.some((known, at) => known !== expanded[at]))
      void setSetting(EXPANDED_TEAMS, JSON.stringify(nextExpanded)).catch(() => {});
  }).catch(() => {});
}

/** Shows or hides a parent row's children, and remembers it across relaunches. */
export function setTeamCollapsed(rowKey: string, collapsed: boolean) {
  setOverride(rowKey, collapsed);
  const id = durable(rowKey);
  if (!id) return;
  pending.delete(id);
  pending.set(id, collapsed);
  if (pending.size > MAX_COLLAPSED_TEAMS) pending.delete(pending.keys().next().value as string);
  save();
}
