import type { Account, FolderOut, HeaderOut, MachineMeta, PathStyle, Profile, Usage } from "../../types";
import { current, fromProjected, period, providerName, severest, windowsAt, type Tone, type UsageWindow } from "../lib/usage";
import { clean, leaf, native, same } from "./paths";
import { trNoop, tr } from "../../i18n";

// The New session dialog's rows, as Qt keeps them in its views
// (tray/src/NewSessionDialog.cpp): computers (loadMachines 487-510),
// projects (setGroups 512-521), folders (loadFolders 536-580), account rows
// with their ranking, labels and limits (updateAccounts 876-952,
// updateUsage 1002-1023, AccountUsage::launchSummary); the Projects
// entries' launch problem is folder-launch.ts's. Everything the core decides
// (the form, Start, the plan) goes through its commands instead.

/** The computers the dialog knows: this computer's card, the configured peers and whose card is ok. */
export interface Fleet {
  local: MachineMeta | undefined;
  /** The configured peers' aliases, in Qt's order (sorted). */
  peers: string[];
  /** The computer ("" or the alias) is online (its card is ok). */
  online(computer: string): boolean;
}

export interface ComputerRow {
  label: string;
  target: string;
  /** Its card is ok: the label reads " (offline)" otherwise (the OS tile's state dot shows the same). */
  online: boolean;
}

/** This computer's host name, "" before it reports one. */
export const localHost = (fleet: Fleet) => fleet.local?.host ?? "";

/** A project the dialog lists: an organization header (store/organization.ts, Qt's setGroups 512-521). */
export type ListedProject = Pick<HeaderOut, "id" | "name" | "color" | "palette" | "accessible">;
/** A project's folders as the dialog copied them (`project_folders`, SessionsWindow.cpp:2923). */
export interface ProjectFolders { folders: readonly FolderOut[] }

/**
 * This computer when it runs local sessions, then the peers, the previous
 * choice and the project's folders' computers (this one only with local
 * sessions; an identified folder's only when its computer is configured).
 * A computer whose card is not ok reads "{computer} (offline)" (ruling D5).
 */
export function computerRows(fleet: Fleet, previous: string, project: ProjectFolders | undefined): ComputerRow[] {
  const host = localHost(fleet);
  // A control-only computer (hgs.exe on Windows) starts sessions on remote machines only.
  const local = fleet.local?.local_sessions !== false;
  const machines: string[] = [];
  if (local) machines.push(host);
  machines.push(...fleet.peers);
  if (previous && !machines.includes(previous)) machines.push(previous);
  for (const folder of project?.folders ?? []) {
    if (!machines.includes(folder.machine) && (folder.machine !== host || local)
      && (!folder.machine_id || folder.machine === host || fleet.peers.includes(folder.machine))) machines.push(folder.machine);
  }
  const rows: ComputerRow[] = [];
  for (const machine of machines) {
    if (!machine || (!local && machine === host)) continue;
    const target = machine === host ? "" : machine;
    const online = fleet.online(target);
    rows.push({ label: machine + (online ? "" : " (offline)"), target, online });
  }
  return rows;
}

/** The previous choice, else the first row; -1 without rows. */
export function computerChoice(rows: ComputerRow[], previous: string): number {
  if (!rows.length) return -1;
  return Math.max(0, rows.findIndex((row) => row.target === previous));
}

export interface ProjectRow { id: string; name: string; color: string | null; palette: number; newProject: boolean }

export const NEW_PROJECT = trNoop("New project…");

/** The accessible projects in the organization's order, plus "New project…" for a repository source. */
export function projectRows(projects: readonly ListedProject[], repository: boolean): ProjectRow[] {
  const rows: ProjectRow[] = projects.filter((p) => p.accessible).map((p) => ({ id: p.id, name: p.name, color: p.color, palette: p.palette, newProject: false }));
  if (repository) rows.push({ id: "", name: tr(NEW_PROJECT), color: null, palette: 0, newProject: true });
  return rows;
}

/** Keep New project…, else the selected project, the initial name, the default project; -1 for none. */
export function projectChoice(rows: ProjectRow[], at: { keepNew: boolean; selected: string; initial: string; fallback: string }): number {
  let index = at.keepNew ? rows.length - 1 : at.selected ? rows.findIndex((row) => !row.newProject && row.id === at.selected) : -1;
  if (index < 0) index = rows.findIndex((row) => row.name === at.initial);
  if (index < 0) index = rows.findIndex((row) => !row.newProject && row.id === at.fallback);
  return index;
}

export interface FolderRow {
  /** The project folder's `FolderOut.key`, "" for a custom path. */
  key: string;
  /** The cleaned path in its computer's stored form. */
  path: string;
  name: string;
  /** The path in its computer's form. */
  tooltip: string;
}

/**
 * The project's folders on `host`, then `custom` (a selected, previous or
 * browsed path) when none of them is it; the index of `custom`, else the
 * first row, else -1.
 */
export function folderRows(project: ProjectFolders | undefined, host: string, local: string, style: PathStyle, custom: string): { rows: FolderRow[]; index: number } {
  const rows: FolderRow[] = [];
  const add = (key: string, raw: string, name: string) => {
    const path = clean(raw, style);
    const last = leaf(path, style);
    rows.push({ key, path, name: name || (last || native(path, style)), tooltip: native(path, style) });
  };
  for (const folder of project?.folders ?? []) {
    if ((folder.machine === local ? "" : folder.machine) !== host) continue;
    add(folder.key, folder.path, folder.name);
  }
  let index = rows.length ? 0 : -1;
  if (custom) {
    index = rows.findIndex((row) => same(row.path, custom, style));
    if (index < 0) {
      add("", custom, "");
      index = rows.length - 1;
    }
  }
  return { rows, index };
}

// Accounts.

/** What the account list chooses by (NewSessionDialog.cpp 897-946). */
export interface AccountChoice {
  /** The dialog's computer: "" or the alias. */
  host: string;
  /** The intended agent (`m_preferredAgent`) and account (`m_preferredAccount`). */
  agent: string;
  account: string;
  /** An explicit or preset account: never silently replaced. */
  preset: boolean;
  /** The row selected now. */
  current: { host: string; provider: string; id: string } | undefined;
  /** This computer's remembered accounts per agent. */
  remembered: Record<string, string>;
  /** The computer's catalog is being read (`m_accountRequest`). */
  loading: boolean;
  style: PathStyle;
  /** The cached `account_status` identity of a raw catalog row. */
  cachedEmail?: (provider: string, id: string) => string;
  /** Unix seconds. */
  now: number;
}

export interface AccountRow {
  /** Provider and id: the row's identity while the catalog's grouping stays. */
  key: string;
  id: string;
  provider: string;
  host: string;
  shell: boolean;
  /** The member the row launches, and its group's members. */
  profile: Profile | undefined;
  members: Profile[];
  /** The group's best usage. */
  usage: Usage | undefined;
  name: string;
  summary: string;
  /** The severest current window's tone, when it is not normal and the values are current. */
  tone: Tone | undefined;
  tooltip: string;
  accessible: string;
}

// Core names compared in their source form, never shown.
const UNNAMED = new Set(["", "Default account", "Native account"]);

/**
 * AccountUsage::launchMeterData: the current windows with a value, used
 * clamped to 0..100; a window with a length shows only its duration. The
 * account row draws them as compact meters, or its summary without any.
 */
export function launchWindows(usage: Usage | undefined, now: number): UsageWindow[] {
  const windows: UsageWindow[] = [];
  for (const projected of usage?.windows ?? []) {
    const window = fromProjected(projected, now);
    if (!current(window) || !Number.isFinite(window.used)) continue;
    windows.push({ ...window, used: Math.max(0, Math.min(100, window.used)), label: window.minutes && window.minutes > 0 ? undefined : window.label });
  }
  return windows;
}

/** AccountUsage::launchSummary: each current window "{period} {n}% used", then balances, joined by two spaces; else "limits n/a". */
export function launchSummary(usage: Usage, now: number): string {
  const limits: string[] = [];
  for (const window of launchWindows(usage, now)) limits.push(period(window) + tr(" %1% used", Math.round(window.used)));
  for (const balance of usage.balances) {
    const amount = balance.balance;
    if (/^\s*[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?\s*$/.test(amount) && Number.isFinite(Number(amount))) limits.push(tr("Balance: %1 %2", amount, balance.currency).trim());
  }
  return limits.length ? limits.join("  ") : tr("limits n/a");
}

/** The row's texts (updateUsage 1002-1023). */
function texts(profile: Profile, usage: Usage | undefined, choice: AccountChoice) {
  let name = usage?.identity.email ?? "";
  if (!name) name = choice.cachedEmail?.(profile.provider, profile.id) ?? "";
  if (!name) name = profile.label;
  if (UNNAMED.has(name)) name = tr("Account n/a");
  const summary = usage ? launchSummary(usage, choice.now) : tr("limits n/a");
  const historical = !!usage && (usage.offline || usage.refresh_error);
  const tone = usage ? severest(windowsAt(usage.windows, choice.now).filter((w) => Number.isFinite(w.used))) : undefined;
  return {
    name, summary, tone: !historical && tone && tone !== "normal" ? tone : undefined,
    tooltip: `${name}\n${summary}`, accessible: `${providerName(profile.provider)} ${name}\n${summary}`,
  };
}

/** The shell's row: "Run shell", or "Run PowerShell" on a Windows computer. */
function shellRow(choice: AccountChoice): AccountRow {
  const name = choice.style === "windows" ? tr("Run PowerShell") : tr("Run shell");
  return {
    key: "sh\n", id: "", provider: "sh", host: choice.host, shell: true, profile: undefined, members: [], usage: undefined,
    name, summary: tr("Interactive terminal session"), tone: undefined, tooltip: "", accessible: name,
  };
}

/**
 * One row per account group, bound to its best-ranked member (preset 5,
 * current 4, remembered 3, default 2, native 1), then the shell. The
 * selection: the preset account only; else the current one, the remembered
 * one, the agent's default, the first. None while the computer's catalog
 * loads with no profiles for another agent than the shell.
 */
export function accountRows(groups: Account[], choice: AccountChoice): { rows: AccountRow[]; selected: number } {
  const currentId = choice.current && choice.current.host === choice.host && choice.current.provider === choice.agent ? choice.current.id : "";
  const rows: AccountRow[] = [];
  for (const group of groups) {
    let chosen: Profile | undefined;
    let best = -1;
    for (const profile of group.members) {
      const rank = choice.preset && profile.provider === choice.agent && profile.id === choice.account ? 5
        : profile.provider === choice.agent && profile.id === currentId ? 4
          : profile.id === choice.remembered[profile.provider] ? 3
            : profile.is_default ? 2 : profile.native ? 1 : 0;
      if (rank > best) { chosen = profile; best = rank; }
    }
    if (!chosen) continue;
    rows.push({
      key: `${chosen.provider}\n${chosen.id}`, id: chosen.id, provider: chosen.provider, host: choice.host, shell: false,
      profile: chosen, members: group.members, usage: group.usage, ...texts(chosen, group.usage, choice),
    });
  }
  let defaultIndex = -1, lastIndex = -1, explicitIndex = -1, currentIndex = -1;
  const last = choice.remembered[choice.agent] ?? "";
  rows.forEach((row, index) => {
    if (row.provider !== choice.agent) return;
    if (defaultIndex < 0 || row.profile?.is_default) defaultIndex = index;
    if (row.id === last) lastIndex = index;
    if (row.id === choice.account) explicitIndex = index;
    if (row.id === currentId) currentIndex = index;
  });
  const profiles = rows.length;
  rows.push(shellRow(choice));
  if (choice.agent === "sh") defaultIndex = lastIndex = explicitIndex = profiles;
  // Explicit selections never silently switch accounts; remembered ones may
  // fall back to the provider's default when the old profile no longer exists.
  const selected = choice.preset ? explicitIndex : currentIndex >= 0 ? currentIndex : lastIndex >= 0 ? lastIndex : defaultIndex >= 0 ? defaultIndex : 0;
  // A temporary shell-only list must not replace the intended agent while
  // its computer's authoritative account catalog is still loading.
  return { rows, selected: choice.loading && profiles === 0 && choice.agent !== "sh" ? -1 : selected };
}

/** New limits change rows in place (refreshUsage 990-1000): each row keeps its member and gets its group's new usage. */
export function refreshRows(rows: AccountRow[], groups: Account[], choice: AccountChoice): AccountRow[] {
  return rows.map((row) => {
    if (row.shell || !row.profile) return row;
    const group = groups.find((g) => g.members.some((m) => m.provider === row.provider && m.id === row.id));
    if (!group) return row;
    const profile = group.members.find((m) => m.provider === row.provider && m.id === row.id) ?? row.profile;
    return { ...row, profile, members: group.members, usage: group.usage, ...texts(profile, group.usage, choice) };
  });
}
