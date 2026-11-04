import { createMemo, createSignal, untrack } from "solid-js";
import { createStore, produce } from "solid-js/store";
import * as bridge from "../bridge";
import { rowId } from "./catalog";
import { clearNotice, notify } from "./actions";
import type { Batch, EditOutcome, GroupingOut, HeaderOut } from "../types";
import type { Session } from "../ui/sessions/types";
import { trNoop, tr } from "../i18n";

// The shared project organization in the Sessions list: each row's project
// and rank and the project headers, as the Rust side sends them (it keeps the
// organization itself), and the organization edits. Every edit is one call,
// which the Rust side sends as one `hgs swarm apply`; its answer is shown
// once. A failed apply stays pending on the Rust side, which sends it again
// on the next tick as Qt's SwarmController does; the webview never repeats a
// call, also not a rejected one.

/** The reserved Inbox project; a drop on the list's blank area goes there (SessionList.cpp:484). */
export const INBOX = "ungrouped";
/** Project sync's notices replace each other and a success clears them, as Qt's one status line. */
export const SYNC_NOTICE = "project-sync";
export const STARTING = trNoop("Project sync is starting.");
export const GONE = trNoop("This project is no longer listed. Open the menu again.");
export const conflictsText = (n: number) => `${n} project conflict(s) need review in Swarm.`;
export const unavailable = (detail: string) => tr("Project sync unavailable: %1", detail);
export const LAYOUT_NOTICE_TITLE = trNoop("Project layout not saved");
export const LAYOUT_NOTICE_TEXT = trNoop("The saved project layout could not be read. The original is kept unchanged. Changes to the project layout in this session will not be saved.");

export const emptyGrouping = (): GroupingOut => ({
  loaded: false, default_project: INBOX, headers: [], ready: false, reason: STARTING,
  busy: false, status: "", status_error: false, conflicts: 0,
});

/** Qt's live and archive member identities (SessionOrganization.cpp:225). */
export const memberOf = (s: Row) => s.state === "archived" ? `${s.rawMachine}\narchive\n${s.archiveId ?? ""}` : `${s.rawMachine}\n${s.name}`;

type Say = (tone: "error" | "warn" | "info", text: string, sticky?: boolean, key?: string, title?: string) => void;
type Row = Pick<Session, "rawMachine" | "name"> & Partial<Pick<Session, "state" | "archiveId">>;
export type Assignment = { group: string; rank: number };

export function createOrganization(say: Say, unsay: (key: string) => void = () => {}, archived: (key: string) => string | undefined = () => undefined) {
  const [grouping, setGrouping] = createSignal<GroupingOut>(emptyGrouping());
  const [groups, setGroups] = createStore<Record<string, Assignment | undefined>>({});
  const defaultProject = createMemo(() => grouping().default_project);
  /** The hub's notice last shown; a view without one clears it. */
  let shownNotice: string | undefined;
  let layoutPauseShown = false;

  /**
   * Applies a batch's grouping after the catalog applied its rows. A replace
   * keeps the assignments of the rows it keeps (grouping does not resend
   * them) and drops the others of that computer; a remove drops its row's.
   */
  function applyGrouping(b: Batch) {
    if (b.grouping) {
      // The Rust side resends a kept edit; once its status is no error any more, the sync notice it raised is stale.
      if (untrack(grouping).status_error && !b.grouping.status_error) unsay(SYNC_NOTICE);
      setGrouping(b.grouping);
      // The window's existing notice bus renders this as a titled sticky
      // toast. Later batches and dismissals do not repeat it this startup.
      if (b.grouping.layout_save_paused && !layoutPauseShown) {
        layoutPauseShown = true;
        say("warn", tr(LAYOUT_NOTICE_TEXT), true, undefined, tr(LAYOUT_NOTICE_TITLE));
      }
      // The hub's one-time warning, shown once until it clears (a dropped draft's reason).
      const notice = b.grouping.notice || undefined;
      if (notice && notice !== shownNotice) say("warn", notice, true);
      shownNotice = notice;
    }
    const assigned = b.groups ?? [];
    if (!assigned.length && !b.removes.length && !b.replaces.length) return;
    setGroups(produce((s) => {
      for (const r of b.replaces) {
        const prefix = rowId(r.computer, "");
        const kept = new Set(r.rows.map((row) => rowId(r.computer, row.key)));
        for (const id of Object.keys(s)) if (id.startsWith(prefix) && !kept.has(id)) delete s[id];
      }
      for (const k of b.removes) delete s[rowId(k.computer, k.key)];
      for (const g of assigned) if (g.archive_request === undefined) s[rowId(g.computer, g.key)] = { group: g.group, rank: g.rank };
    }));
  }

  const groupOf = (id: string) => groups[id]?.group ?? archived(id) ?? defaultProject();
  const headerOf = (project: string): HeaderOut | undefined => grouping().headers.find((h) => h.id === project);

  /** Why an edit cannot run now (the disabled entries' reason). */
  function refusal(...projects: (string | undefined)[]): string | undefined {
    const g = grouping();
    if (!g.ready) return g.reason ?? tr(STARTING);
    if (projects.some((p) => p !== undefined && !headerOf(p))) return tr(GONE);
    return undefined;
  }

  function report(out: EditOutcome) {
    switch (out.outcome) {
      // One project sync notice, as Qt's status line: the next outcome replaces it, a clean success clears it.
      case "done": if (out.conflicts > 0) say("warn", conflictsText(out.conflicts), true, SYNC_NOTICE); else unsay(SYNC_NOTICE); return;
      // Qt's status line for the failure (SwarmController.cpp:69, 195); the Rust side resends.
      case "kept": say("error", out.detail, true, SYNC_NOTICE); return;
      case "refused": say("info", out.detail); return;
    }
  }

  async function run(why: string | undefined, call: () => Promise<EditOutcome>): Promise<EditOutcome> {
    let out: EditOutcome;
    if (why) out = { outcome: "refused", detail: why };
    else {
      try { out = await call(); } catch (error) {
        const detail = typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
        out = { outcome: "kept", detail: unavailable(detail) };
      }
    }
    report(out);
    return out;
  }

  const invalidArchive = (rows: readonly Row[]) => rows.some((r) => r.state === "archived" && !r.archiveId)
    ? tr("Invalid archived session identity. Refresh the archive.") : undefined;

  /** Move to project: one call for every row, as one `swarm apply` (zerus-5wmh.18, .20). */
  function moveToProject(rows: readonly Row[], projectId: string, before?: Row): Promise<EditOutcome> {
    if (!rows.length) return Promise.resolve({ outcome: "refused", detail: tr("Choose the sessions to move.") });
    return run(invalidArchive(before ? [...rows, before] : rows) ?? refusal(projectId), () => bridge.moveToProject(rows.map(memberOf), projectId, before ? memberOf(before) : null));
  }
  const createProject = (name: string, rows: readonly Row[] = []) =>
    run(invalidArchive(rows) ?? refusal(), () => bridge.createProject(name, rows.map(memberOf)));
  const setProjectColor = (projectId: string, color: string) => run(refusal(projectId), () => bridge.setProjectColor(projectId, color));
  const renameProject = (projectId: string, name: string) => run(refusal(projectId), () => bridge.renameProject(projectId, name));
  const deleteProject = (projectId: string, destination: string) =>
    run(refusal(projectId, destination), () => bridge.deleteProject(projectId, destination));
  const moveProject = (projectId: string, before: string | undefined) => run(refusal(projectId), () => bridge.moveProject(projectId, before ?? null));
  const sortProject = (projectId: string) => run(refusal(projectId), () => bridge.sortProject(projectId));
  async function setDefaultProject(projectId: string): Promise<boolean> {
    const why = refusal(projectId);
    if (why) { say("info", why); return false; }
    try { await bridge.setDefaultProject(projectId); return true; }
    catch (error) { say("error", String(error), true); return false; }
  }
  const changeProjectFolder = (project: string, change: import("../ui/screens/project-folders").FolderChange) => run(refusal(project, change.kind === "transfer" ? change.destination : undefined), () => bridge.projectFolderChange(project, change));
  /** Expand or collapse: this device's layout; the next batch shows it. */
  const setProjectCollapsed = (projectId: string, collapsed: boolean) => { void bridge.setProjectCollapsed(projectId, collapsed).catch(() => {}); };

  return { grouping, groups, defaultProject, applyGrouping, groupOf, headerOf, moveToProject, createProject,
           renameProject, setProjectColor, deleteProject, moveProject, sortProject, setProjectCollapsed, setDefaultProject, changeProjectFolder };
}

export type Organization = ReturnType<typeof createOrganization>;

let archivedGroup: (key: string) => string | undefined = () => undefined;
/** Loaded archive rows own their bounded placement assignments. */
export function bindArchivedGroups(lookup: (key: string) => string | undefined) { archivedGroup = lookup; }

const app = createOrganization(notify, clearNotice, (key) => archivedGroup(key));
export const { grouping, groups, defaultProject, applyGrouping, groupOf, headerOf, moveToProject, createProject,
               renameProject, setProjectColor, deleteProject, moveProject, sortProject, setProjectCollapsed, setDefaultProject, changeProjectFolder } = app;

/** Project settings… asks the Projects screen to show this project. */
export const [projectToShow, setProjectToShow] = createSignal<string | undefined>();
