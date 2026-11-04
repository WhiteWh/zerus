import { dayTime } from "../lib/usage";
import type { CreatedWorktree } from "../../types";
import { array, object, string, type Json } from "../lib/json";
import { composing } from "../lib/keys";
import { absolute, clean, join, sameKey, split, type Style } from "./paths";
import { tr } from "../../i18n";

// New session's two worktree dialogs, as Qt keeps them in their views:
// Choose existing… is WorktreePanel in picker mode (tray/src/WorktreePanel.h)
// and Create manually… is NewWorktreeDialog (tray/src/NewWorktreeDialog.h).
// WorktreePicker.tsx and NewWorktreeDialog.tsx render them. The picker only
// reads (`worktrees --path P --json [--refresh]`); Create worktree is the one
// call that creates, sent once per click with no expected machine, as Qt
// sends it. Answers for another request change nothing.

const errorText = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));
const flag = (value: Json, key: string) => value[key] === true;

// Choose existing… (WorktreePanel in picker mode).

/** A listed session of the computer, as the picker counts it (G1). */
export interface PickerSession { gitRoot: string; cwdCanonical: string }

/** Whether two session lists hold the same paths in the same order (the picker then keeps its counts). */
export const sameSessions = (a: readonly PickerSession[], b: readonly PickerSession[]) =>
  a.length === b.length && a.every((session, i) => session.gitRoot === b[i].gitRoot && session.cwdCanonical === b[i].cwdCanonical);

/** The computer's sessions per checkout (`gitRoot`) and per folder (`cwdCanonical`), keyed as `same` compares. */
interface Counts { sessions: readonly PickerSession[]; style: Style; roots: Map<string, number>; folders: Map<string, number> }

/** One checkout or folder the picker lists. */
export interface WorktreeRow {
  path: string;
  sessions: number;
  /** "Main checkout / main (1 session)" and the like. */
  label: string;
  /** The path, then the locked and prunable reasons, one per line. */
  tooltip: string;
  /** Available, not bare and current: Use folder takes it. */
  usable: boolean;
}

export interface WorktreePickerView {
  status: string;
  /** Fresh display data only; cleanup still rechecks through its native review. */
  fresh: boolean;
  reading: boolean;
  rows: WorktreeRow[];
  /** The selected row's path; undefined without rows. */
  selected: string | undefined;
  refreshEnabled: boolean;
  useEnabled: boolean;
}

/** The catalog lists at most this many worktrees (Partial catalog). */
export const MAX_WORKTREES = 512;
/** The same bounded catalog/fallback folder in picker and normal panel mode. */
export function catalogRoots(data: Json, path: string): Json[] {
  const roots = array(data.worktrees).slice(0, MAX_WORKTREES).map(object);
  return roots.length || !path ? roots : [{ path: typeof data.path === "string" ? data.path : path, kind: "folder", available: string(data.state) !== "folder_unavailable" && Object.keys(data).length > 0 }];
}
/** Qt's "d MMM HH:mm:ss" in local time, with C-locale month names. */
export const snapshotTime = (seconds: number): string => dayTime(seconds, true);

/** Shared WorktreePanel status for the picker and the session side panel. */
export function catalogStatus(path: string, machine: string, data: Json, online: boolean, reading: boolean, error = ""): string {
  const state = string(data.state), stale = !online || flag(data, "stale") || error !== "";
  let status: string;
  if (!path) status = tr("Select a folder.");
  else if (!online) status = tr("Offline. Showing the last snapshot.");
  else if (error) status = tr("Catalog unavailable: %1", error);
  else if (reading) status = tr("Reading folder…");
  else if (state === "not_repo") status = tr("Ordinary folder (no Git).");
  else if (state !== "ok") status = state === "folder_unavailable" ? tr("Folder is missing or inaccessible.") : tr("Git information unavailable. You can still use an accessible folder.");
  else if (flag(data, "partial")) status = tr("Partial catalog (first 512 worktrees).");
  else status = tr("Existing working folders on %1", machine);
  const sampled = typeof data.sampled_at === "number" ? data.sampled_at : 0;
  if (sampled > 0) status += tr("\nSnapshot: %1%2", snapshotTime(sampled), stale ? tr(" (outdated)") : "");
  return status;
}

/** A row's label (WorktreePanel::render): its kind, branch, marks and its session count. */
export function worktreeLabel(row: Json, sessions: number): string {
  const kind = string(row.kind);
  let label = kind === "main" ? tr("Main checkout") : kind === "linked" ? tr("Linked worktree") : kind === "bare" ? tr("Bare repository") : tr("Folder");
  const branch = string(row.branch);
  if (branch) label += ` / ${branch}`;
  else if (flag(row, "detached")) label += tr(" / detached %1", string(row.head).slice(0, 8));
  if (flag(row, "locked")) label += tr(" (locked)");
  if (flag(row, "prunable")) label += tr(" (prunable)");
  if (!flag(row, "available")) label += tr(" (unavailable)");
  return label + (sessions === 1 ? tr(" (1 session)") : tr(" (%1 sessions)", sessions));
}

/**
 * The picker of one computer's folder, fixed for its life (`setContext` at
 * open). It shows the folder's snapshot, then reads its catalog once it is
 * shown on an online computer; Refresh reads it again. `online`, the
 * computer's sessions and its style are the caller's at each view.
 */
export class WorktreePicker {
  onChange?: () => void;

  private data: Json = {};
  private error = "";
  private request = 0;
  private ticket = 0;
  private answered = false;
  private selection: string | undefined;
  private closed = false;
  private counted: Counts | undefined;
  private listed: { data: Json; stale: boolean; counts: Counts; rows: WorktreeRow[] } | undefined;

  constructor(
    private readonly calls: { snapshot(): Promise<Json>; catalog(refresh: boolean): Promise<Json> },
    private readonly path: string,
    /** The computer as the status names it: the alias, else this computer's host name. */
    private readonly machine: string,
  ) {
    calls.snapshot().then((saved) => {
      if (this.closed || this.answered) return;
      this.data = object(saved);
      this.changed();
    }, () => {});
  }

  /** Shown (`showEvent`) or Refresh: reads the catalog unless no folder, an offline computer or a read on its way. */
  private read(online: boolean, refresh: boolean) {
    if (this.path === "" || !online || this.request) return;
    this.error = "";
    const request = ++this.ticket;
    this.request = request;
    this.calls.catalog(refresh).then((data) => {
      if (this.closed || request !== this.request) return;
      this.request = 0;
      this.answered = true;
      this.data = object(data);
      this.error = "";
      this.changed();
    }, (failure) => {
      if (this.closed || request !== this.request) return;
      this.request = 0;
      this.error = errorText(failure);
      this.changed();
    });
    this.changed();
  }

  show(online: boolean) { this.read(online, false); }
  refresh(online: boolean) { this.read(online, true); }

  catalog(): Json { return this.data; }

  select(path: string) {
    this.selection = path;
    this.changed();
  }

  dispose() { this.closed = true; }

  view(online: boolean, sessions: readonly PickerSession[], style: Style): WorktreePickerView {
    const data = this.data;
    const stale = !online || flag(data, "stale") || this.error !== "";
    const status = catalogStatus(this.path, this.machine, data, online, !!this.request, this.error);

    const rows = this.rows(data, stale, this.counts(sessions, style));
    // The kept choice, else the opened folder or its root (the last such row), else the first.
    let selected = rows.find((r) => r.path === this.selection);
    if (!selected && this.selection === undefined) {
      const root = string(data.selected_root);
      selected = rows.filter((r) => r.path === this.path || r.path === root).at(-1);
    }
    selected ??= rows[0];
    const current = online && !this.request && this.error === "" && !flag(data, "stale");
    return {
      status, rows, selected: selected?.path, fresh: current, reading: !!this.request,
      refreshEnabled: online && !this.request && this.path !== "",
      useEnabled: !!selected?.usable && current,
    };
  }

  /**
   * The sessions per checkout and folder, counted in one pass when the
   * computer's sessions or its style change, never on a selection change.
   */
  private counts(sessions: readonly PickerSession[], style: Style): Counts {
    const known = this.counted;
    if (known && known.sessions === sessions && known.style === style) return known;
    const roots = new Map<string, number>();
    const folders = new Map<string, number>();
    const add = (map: Map<string, number>, path: string) => { const key = sameKey(path, style); map.set(key, (map.get(key) ?? 0) + 1); };
    for (const session of sessions) {
      add(roots, session.gitRoot);
      add(folders, session.cwdCanonical);
    }
    this.counted = { sessions, style, roots, folders };
    return this.counted;
  }

  /** The rows (WorktreePanel::render): built again only for new data, counts or staleness. */
  private rows(data: Json, stale: boolean, counts: Counts): WorktreeRow[] {
    const known = this.listed;
    if (known && known.data === data && known.stale === stale && known.counts === counts) return known.rows;
    const roots = catalogRoots(data, this.path);
    const rows = roots.map((row): WorktreeRow => {
      const path = string(row.path);
      const count = (row.kind !== "folder" ? counts.roots : counts.folders).get(sameKey(path, counts.style)) ?? 0;
      return {
        path, sessions: count, label: worktreeLabel(row, count),
        tooltip: `${path}\n${string(row.locked_reason)}\n${string(row.prunable_reason)}`,
        usable: flag(row, "available") && row.kind !== "bare" && !stale,
      };
    });
    this.listed = { data, stale, counts, rows };
    return rows;
  }

  private changed() { if (!this.closed) this.onChange?.(); }
}

// Create manually… (NewWorktreeDialog).

type KeyPress = Pick<KeyboardEvent, "key" | "altKey" | "ctrlKey" | "metaKey" | "shiftKey"> & Partial<Pick<KeyboardEvent, "isComposing" | "keyCode">>;

/**
 * Start from's keys with its list closed (QComboBox::keyPressEvent, Qt 6.12,
 * on an editable combo box): Alt+Down and a plain F4 open the list; Up and
 * PageUp step up, Down and PageDown step down, one item each; Control
 * (Command on macOS) with an arrow and every other key go to the text.
 */
export function baseKey(e: KeyPress): "open" | "previous" | "next" | "text" {
  if (composing(e)) return "text";
  const command = e.ctrlKey || e.metaKey;
  if ((e.key === "ArrowDown" && e.altKey) || (e.key === "F4" && !e.altKey && !command && !e.shiftKey)) return "open";
  if ((e.key === "ArrowUp" && !command) || e.key === "PageUp") return "previous";
  if ((e.key === "ArrowDown" && !command) || e.key === "PageDown") return "next";
  return "text";
}

/** The branch as a folder name: runs of anything but letters, digits, '.', '_' and '-' become '-'. */
export const branchFolder = (branch: string) => branch.trim().replace(/[^\p{L}\p{N}._-]+/gu, "-");

export interface NewWorktreeView {
  context: string;
  branch: string;
  base: string;
  /** Start from's list: HEAD, then the catalog's branches. */
  bases: string[];
  /** Start from's current item (QComboBox's currentIndex): the typed text's item, else the last one chosen. */
  baseIndex: number;
  destination: string;
  /** "", "Creating worktree…" or the failure. */
  error: string;
  busy: boolean;
  createEnabled: boolean;
}

/**
 * New worktree for one computer and folder, fixed for its life. The folder
 * suggestion is `{parent of the selected root}/{repository}-{branch}` in the
 * computer's path style (deviation 3) until the folder is edited.
 */
export class NewWorktree {
  onChange?: () => void;
  /** Create worktree succeeded: the new checkout and its verified catalog. */
  onCreated?: (created: CreatedWorktree) => void;

  private branch: string;
  private base = "HEAD";
  private chosenBase = 0;
  private destination = "";
  private custom = false;
  private parent: string;
  private repository: string;
  private error = "";
  private request = 0;
  private ticket = 0;
  private readonly bases: string[];

  constructor(
    private readonly create_: (branch: string, base: string, destination: string) => Promise<CreatedWorktree>,
    private readonly machine: string,
    private readonly source: string,
    catalog: Json,
    branch: string,
    private readonly style: Style,
  ) {
    this.branch = branch;
    this.bases = ["HEAD"];
    for (const entry of array(catalog.worktrees)) {
      const name = string(object(entry).branch);
      if (name && !this.bases.includes(name)) this.bases.push(name);
    }
    const root = string(catalog.selected_root) || source;
    [this.parent, this.repository] = split(root, style);
    this.suggest();
  }

  view(): NewWorktreeView {
    const busy = this.request !== 0;
    return {
      context: tr("Computer: %1\nRepository: %2", this.machine, this.source),
      branch: this.branch, base: this.base, bases: this.bases, baseIndex: this.baseIndex(), destination: this.destination, error: this.error, busy,
      createEnabled: !busy && this.branch.trim() !== "" && this.base.trim() !== "" && absolute(this.destination, this.style),
    };
  }

  /** Closing is refused while the worktree is being created. */
  canClose() { return this.request === 0; }

  setBranch(text: string) {
    if (this.request) return;
    this.branch = text;
    this.suggest();
    this.changed();
  }

  /** Start from typed: the current item stays the last one chosen unless the text names another. */
  setBase(text: string) {
    if (this.request) return;
    this.base = text;
    this.changed();
  }

  /** Start from's list chose item `index`. */
  chooseBase(index: number) {
    if (this.request || index < 0 || index >= this.bases.length) return;
    this.chosenBase = index;
    this.base = this.bases[index];
    this.changed();
  }

  /**
   * Up or PageUp (-1), Down or PageDown (+1) with the list closed
   * (QComboBox::keyPressEvent): one item from the current one, none past
   * either end.
   */
  stepBase(by: 1 | -1) {
    const to = this.baseIndex() + by;
    if (to >= 0 && to < this.bases.length) this.chooseBase(to);
  }

  /** The folder edited by hand: the suggestion stops following the branch. */
  setDestination(text: string) {
    if (this.request) return;
    this.custom = true;
    this.destination = text;
    this.changed();
  }

  /** Where Parent folder… starts: the folder's parent. */
  browseStart() { return split(this.destination, this.style)[0]; }

  /**
   * Parent folder… chose `folder`: the suggestion follows the branch again
   * under it. The folder takes the computer's stored form first ('/' on
   * Windows too, as `QDir::filePath` gives), so the suggestion has one form.
   */
  browsed(folder: string) {
    if (!folder || this.request) return;
    this.parent = clean(folder, this.style);
    this.custom = false;
    this.suggest();
    this.changed();
  }

  /** Create worktree: one request; its failure is shown, never sent again. */
  create() {
    if (!this.view().createEnabled) return;
    this.error = tr("Creating worktree…");
    const request = ++this.ticket;
    this.request = request;
    this.create_(this.branch.trim(), this.base.trim(), this.destination).then((created) => {
      if (request !== this.request) return;
      this.request = 0;
      this.changed();
      this.onCreated?.(created);
    }, (failure) => {
      if (request !== this.request) return;
      this.request = 0;
      this.error = errorText(failure);
      this.changed();
    });
    this.changed();
  }

  private baseIndex() {
    const at = this.bases.indexOf(this.base);
    return at >= 0 ? at : this.chosenBase;
  }

  private suggest() {
    if (this.custom) return;
    this.destination = join(this.parent, `${this.repository}-${branchFolder(this.branch)}`);
  }

  private changed() { this.onChange?.(); }
}
