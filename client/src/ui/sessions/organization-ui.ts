import { createSignal } from "solid-js";
import type { EditOutcome, GroupingOut, HeaderOut } from "../../types";
import type { MenuEntry } from "../../store/ui";
import type { Session } from "./types";
import { tr } from "../../i18n";

// The rules of the project menus and dialogs (SessionsWindow.cpp:2558-2616,
// 5363-5372; ProjectsDialog.cpp:367-381): pure rules, the header menu's
// entries with their actions passed in, and the dialogs' controller, which
// takes the organization store. The components only render them.

export interface Entry { enabled: boolean; reason?: string }

interface HeaderMenuState {
  /** The project is collapsed now; the entry expands it. Logic reads this, never the label. */
  collapsed: boolean;
  collapse: string;
  edit: Entry;
  up: Entry & { before?: string };
  down: Entry & { before?: string };
  remove: Entry;
}

/** An entry that writes: enabled once project sync is ready, else its reason is the hint (Qt's `hint`). */
export function editEntry(g: GroupingOut): Entry {
  return g.ready || g.reason === null ? { enabled: g.ready } : { enabled: false, reason: g.reason };
}

export function headerMenuState(g: GroupingOut, project: string): HeaderMenuState | undefined {
  const at = g.headers.findIndex((h) => h.id === project);
  if (at < 0) return undefined;
  const edit = editEntry(g);
  const several = g.headers.length > 1;
  return {
    collapsed: g.headers[at].collapsed,
    collapse: g.headers[at].collapsed ? tr("Expand project") : tr("Collapse project"),
    edit,
    up: { enabled: edit.enabled && at > 0, before: g.headers[at - 1]?.id, reason: edit.reason },
    // Before the project after the next one, else to the end (2598-2600).
    down: { enabled: edit.enabled && at + 1 < g.headers.length, before: g.headers[at + 2]?.id, reason: edit.reason },
    // Qt gives no reason for the last project (2614).
    remove: { enabled: edit.enabled && several, reason: several ? edit.reason : undefined },
  };
}

/** What the header menu's entries do; `collapse` gets the new state, `move` the project to go before. */
export interface HeaderActions {
  /** New session on this project (showGroupMenu 2572-2573). */
  newSession: () => void;
  filter: () => void;
  collapse: (collapsed: boolean) => void;
  rename: () => void;
  settings: () => void;
  move: (before: string | undefined) => void;
  sort: () => void;
  remove: () => void;
}

/** The project header's menu, in Qt's order (2568-2616); none for a project no longer listed. */
export function headerEntries(g: GroupingOut, project: string, act: HeaderActions): MenuEntry[] {
  const m = headerMenuState(g, project);
  if (!m) return [];
  const hint = (e: Entry) => (e.enabled ? undefined : e.reason);
  return [
    { label: tr("New session…"), icon: "plus", action: act.newSession },
    { separator: true },
    { label: tr("Filter this project"), icon: "funnel", action: act.filter },
    { separator: true },
    // This device's layout: never waits for sync.
    { label: m.collapse, icon: "list-tree", action: () => act.collapse(!m.collapsed) },
    { label: tr("Rename project…"), icon: "pencil", disabled: !m.edit.enabled, hint: hint(m.edit), action: act.rename },
    { label: tr("Project settings…"), icon: "settings", action: act.settings },
    { separator: true },
    { label: tr("Move project up"), icon: "arrow-up", disabled: !m.up.enabled, hint: hint(m.up), action: () => act.move(m.up.before) },
    { label: tr("Move project down"), icon: "arrow-down", disabled: !m.down.enabled, hint: hint(m.down), action: () => act.move(m.down.before) },
    { label: tr("Sort sessions by name"), icon: "list-filter", disabled: !m.edit.enabled, hint: hint(m.edit), action: act.sort },
    { separator: true },
    { label: tr("Delete project…"), icon: "trash", danger: true, disabled: !m.remove.enabled, hint: hint(m.remove), action: act.remove },
  ];
}

/** Every other project; the default one is chosen, else the first (ProjectsDialog.cpp:373-374). */
export function deleteTargets(g: GroupingOut, project: string): { options: HeaderOut[]; selected: string } {
  const options = g.headers.filter((h) => h.id !== project);
  return { options, selected: options.find((h) => h.id === g.default_project)?.id ?? options[0]?.id ?? "" };
}

/** Every project, checked when every chosen row is in it (5365-5366). */
export function moveChoices(g: GroupingOut, current: readonly string[]): (HeaderOut & { current: boolean })[] {
  return g.headers.map((h) => ({ ...h, current: current.length > 0 && current.every((p) => p === h.id) }));
}

export const projectColor = (h: Pick<HeaderOut, "color" | "palette">) => h.color ?? `var(--z-entity-palette-${h.palette})`;

/** A Sessions list filter chip: a machine, or a project by its id. */
export interface FilterChip { id: string; label: string; kind: "m" | "p" | "a"; provider?: string }
/**
 * A project chip matches its header band; a machine chip, or a project no
 * longer listed, takes its name's palette slot (`slot`, SessionCard's paletteIndex).
 */
export function chipColor(c: FilterChip, headerOf: (project: string) => Pick<HeaderOut, "color" | "palette"> | undefined,
                          slot: (name: string) => number): string {
  const h = c.kind === "p" ? headerOf(c.id) : undefined;
  if (c.kind === "a") return "var(--z-entity-palette-6)";
  return h ? projectColor(h) : `var(--z-entity-palette-${slot(c.label)})`;
}

/**
 * createGroup and renameGroup ignore a blank name (SessionOrganization.cpp:135-136, 149).
 * Qt's name dialogs set no length limit (QInputDialog::getText, SessionsWindow.cpp:2561,
 * 2585); the core trims a name and keeps 80 characters (`swarm::edit::clean_name`).
 */
export const nameProblem = (name: string) => (name.trim() ? undefined : tr("Enter a project name."));
export const deleteText = (name: string) =>
  tr("Delete “%1”? Choose the project that will receive its sessions. Files and running agents stay in place.", name);

/** The organization store's part the dialogs use (store/organization.ts). */
export interface OrgStore {
  grouping: () => GroupingOut;
  groupOf: (id: string) => string;
  headerOf: (project: string) => HeaderOut | undefined;
  moveToProject: (rows: readonly Session[], project: string) => Promise<EditOutcome>;
  createProject: (name: string, rows: readonly Session[]) => Promise<EditOutcome>;
  renameProject: (project: string, name: string) => Promise<EditOutcome>;
  deleteProject: (project: string, destination: string) => Promise<EditOutcome>;
}

export type OrgDialog =
  | { kind: "move"; rows: Session[] }
  | { kind: "new"; rows: Session[] }
  | { kind: "rename"; project: string; name: string }
  | { kind: "delete"; project: string; name: string; options: HeaderOut[]; selected: string };

/**
 * The organization dialogs, one open at a time: Move to project (for one row
 * or a selection), New project, Rename project and Delete project. Each
 * confirmation closes its dialog and makes one edit call, whose outcome the
 * store reports.
 */
export function createOrgDialogs(org: OrgStore) {
  const [dialog, setDialog] = createSignal<OrgDialog | undefined>(undefined, { equals: false });
  const close = () => setDialog(undefined);

  const openMove = (rows: readonly Session[]) => { setDialog({ kind: "move", rows: [...rows] }); };
  const openNew = (rows: readonly Session[] = []) => { setDialog({ kind: "new", rows: [...rows] }); };
  /** Qt asks only while the project is listed (SessionsWindow.cpp:2584). */
  function openRename(project: string) {
    const h = org.headerOf(project);
    if (h) setDialog({ kind: "rename", project, name: h.name });
  }
  /** Qt asks only while the project is listed and another can take its sessions (ProjectsDialog.cpp:369). */
  function openDelete(project: string) {
    const g = org.grouping();
    const h = org.headerOf(project);
    if (!h || g.headers.length < 2) return;
    setDialog({ kind: "delete", project, name: h.name, ...deleteTargets(g, project) });
  }

  /** The Move dialog's projects, by where the organization puts each row. */
  const moveChoicesFor = (rows: readonly Session[]) => moveChoices(org.grouping(), rows.map((s) => org.groupOf(s.key)));
  /** A project chosen in the Move dialog: every row in one call, also to the project they are in (5367-5369). */
  function choose(project: string) {
    const d = dialog();
    if (d?.kind !== "move") return;
    close();
    void org.moveToProject(d.rows, project);
  }
  /** The Move dialog's New project…: its rows go into the new project in the same apply (5371, 2565). */
  function newProject() {
    const d = dialog();
    openNew(d?.kind === "move" ? d.rows : []);
  }

  /** The name dialog's title and starting text. */
  function nameDialog(): { title: string; initial: string } | undefined {
    const d = dialog();
    if (d?.kind === "new") return { title: tr("New project"), initial: "" };
    if (d?.kind === "rename") return { title: tr("Rename project"), initial: d.name };
    return undefined;
  }
  /** OK in New or Rename project; a blank name keeps the dialog open. */
  function submitName(name: string): boolean {
    const d = dialog();
    if ((d?.kind !== "new" && d?.kind !== "rename") || nameProblem(name)) return false;
    close();
    if (d.kind === "new") void org.createProject(name, d.rows);
    else void org.renameProject(d.project, name);
    return true;
  }

  /** "Move sessions and delete" with the chosen project. */
  function confirmDelete(destination: string) {
    const d = dialog();
    if (d?.kind !== "delete" || !destination) return;
    close();
    void org.deleteProject(d.project, destination);
  }

  /** The session menu's Move to project entry, also for archive identities. */
  function moveEntry(s: Session): MenuEntry {
    const e = editEntry(org.grouping());
    return { label: tr("Move to project…"), icon: "folder", disabled: !e.enabled, hint: e.reason, action: () => openMove([s]) };
  }

  return { dialog, close, openMove, openNew, openRename, openDelete, moveChoicesFor, choose, newProject, nameDialog, submitName,
           confirmDelete, moveEntry };
}
