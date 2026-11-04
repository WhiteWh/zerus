import { array, object, string, type Json } from "../lib/json";
import { composing } from "../lib/keys";
import { trNoop, tr } from "../../i18n";

// The folder browser of another computer: a port of the peer branch of
// tray/src/DirectoryDialog.cpp (46-326). This computer's folders use the
// system dialog (`pick_local_folder`), never this. Listing (`dirs PATH`)
// only reads; New folder (`dirs --create`) is the one call that creates,
// named by a click, sent once under its own identity. Each listing carries
// its request number: a late one for a folder left since is dropped, and so
// is every listing while a folder is being created. A lost creation is read
// again by its path, never sent again (156-160).
//
// FolderBrowser.tsx renders it; New session's Browse…, Clone repository and
// Projects' Add folder share it.

/** One listed folder: its name and its path on that computer. */
export interface DirectoryEntry { name: string; path: string }

/** What the browser asks of its computer (`list_directories`, `create_directory`). */
export interface FolderBrowserCalls {
  list(path: string, hidden: boolean): Promise<Json>;
  create(path: string): Promise<Json>;
}

/** The inline New folder row (222-228). */
export interface InlineFolder { name: string; createEnabled: boolean }

/** The New folder dialog (297-306). */
export interface FolderPrompt { title: string; text: string; name: string; createEnabled: boolean }

export interface FolderBrowserView {
  title: string;
  machine: string;
  /** The path field's text. */
  path: string;
  /** The folder the last listing showed, "" before one or after a failure. */
  current: string;
  /** The listed folders the filter keeps, in order. */
  entries: DirectoryEntry[];
  filter: string;
  hidden: boolean;
  status: string;
  /** The selected folder's path; undefined for none. */
  selected: string | undefined;
  inline: InlineFolder | undefined;
  prompt: FolderPrompt | undefined;
  /** Up, Home, Go, the path field, the filter, Show hidden folders and Cancel. */
  controlsEnabled: boolean;
  listEnabled: boolean;
  newFolderEnabled: boolean;
  dialogEnabled: boolean;
  chooseEnabled: boolean;
  chooseText: string;
  creating: boolean;
}

export const USE_THIS: string = trNoop("Use this folder");
export const USE_SELECTED: string = trNoop("Use selected folder");
export const LOADING = trNoop("Loading folders…");
export const TYPED = trNoop("Press Go to open this path, or New folder to create it.");
export const AVAILABLE = trNoop("The folder is available. Press Use this folder to select it.");
export const CREATED = trNoop("Folder created. Press Use this folder to select it.");
export const TRUNCATED = trNoop("Showing the first 2,000 folders. You can also enter an exact path above.");
export const LISTED = "Double-click to open a folder, or select it and press Use selected folder.";
export const EMPTY = trNoop("No subfolders. You can use this folder.");
/** `hgs dirs` lists at most this many folders; a longer reply keeps its first ones. */
export const MAX_ENTRIES = 2000;

// Enter (DirectoryDialog.cpp 83-85, 95, 162, 175-181, 303-311). The
// dialog's QDialogButtonBox makes its first AcceptRole button the default
// when it shows, although the code turns default and auto-default off: Use
// this folder in the browser, Create folder in the New folder prompt. Every
// other button has auto-default off, so Enter goes past it to the dialog,
// which presses the default button when it is enabled, and only without
// modifiers. The path field presses Go, the list opens its current folder,
// the inline name presses Create, and the list swallows Enter on the inline
// row's buttons. The context menu (a QMenu) triggers its active entry.

/** A key press as the mappings read it. */
export type EnterPress = Pick<KeyboardEvent, "key" | "altKey" | "ctrlKey" | "metaKey" | "shiftKey"> & Partial<Pick<KeyboardEvent, "isComposing" | "keyCode">>;

/** Where Enter lands in the browser. */
export type BrowserPlace =
  /** The path field. */
  | "path"
  /** The list itself (its own handler opens the current folder). */
  | "list"
  /** The inline New folder row's name. */
  | "inline-name"
  /** The inline row's Create and Cancel buttons. */
  | "inline"
  /** The list's context menu. */
  | "menu"
  /** Up, Home, Go, the filter, Show hidden folders and the other buttons. */
  | "other";

/**
 * What Enter does in the browser:
 * - "go": Go (the path field);
 * - "create": Create (the inline name, which checks the name);
 * - "choose": the default button, Use this folder or Use selected folder;
 * - "ignore": nothing, not even the focused button's own press;
 * - "native": the control's own handling (the list, the context menu's entry, an input method).
 */
export type BrowserEnter = "go" | "create" | "choose" | "ignore" | "native";

const plain = (e: EnterPress) => !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;

/** One entry of the list's context menu. */
export interface MenuEntry { label: string; enabled: boolean; action: "new" | "dialog" }

/**
 * The list's context menu (92-99): New folder as its button is, and the
 * always enabled New folder dialog…. None while the list is disabled: a
 * disabled QListWidget never asks for its menu.
 */
export function listMenu(view: Pick<FolderBrowserView, "listEnabled" | "newFolderEnabled">): MenuEntry[] | undefined {
  if (!view.listEnabled) return undefined;
  return [{ label: tr("New folder"), enabled: view.newFolderEnabled, action: "new" }, { label: tr("New folder dialog…"), enabled: true, action: "dialog" }];
}

/** Enter in the browser; `chooseEnabled` is the default button's state. */
export function browserEnter(place: BrowserPlace, e: EnterPress, view: Pick<FolderBrowserView, "chooseEnabled" | "controlsEnabled">): BrowserEnter {
  if (e.key !== "Enter" || composing(e)) return "native";
  switch (place) {
    case "path": return view.controlsEnabled ? "go" : "ignore";
    case "list": case "menu": return "native";
    case "inline-name": return "create";
    case "inline": return "ignore";
    case "other": return plain(e) && view.chooseEnabled ? "choose" : "ignore";
  }
}

/** Where Enter lands in the New folder prompt. */
export type PromptPlace = "name" | "create" | "cancel";

/**
 * Enter in the New folder prompt: in the name and on Create folder it
 * creates (the prompt checks the name). On Cancel it cancels, as a click
 * does; Qt's default button would take that Enter and create the folder
 * (a listed deviation, ruled 2026-10-29). Modifiers stop the buttons' Enter.
 */
export function promptEnter(place: PromptPlace, e: EnterPress): "create" | "cancel" | "ignore" | "native" {
  if (e.key !== "Enter" || composing(e)) return "native";
  if (place === "name") return "create";
  if (!plain(e)) return "ignore";
  return place === "cancel" ? "cancel" : "create";
}

const utf8Length = (text: string) => new TextEncoder().encode(text).length;
const errorText = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));

/** `validFolderName` (25-30): not empty, not `.` or `..`, no `/`, no control character, the whole path at most 4096 bytes. */
export function validFolderName(name: string, parent: string): boolean {
  return name !== "" && name !== "." && name !== ".." && !name.includes("/")
    && utf8Length(`${parent}/${name}`) <= 4096 && !/\p{Cc}/u.test(name);
}

/** `childPath` (31-34). */
export const childPath = (parent: string, name: string) => `${parent}${parent.endsWith("/") ? "" : "/"}${name}`;

/**
 * One folder browser on one computer. `computer` names it in the texts; the
 * calls are already bound to it. Every change calls `onChange`.
 */
export class FolderBrowser {
  onChange?: () => void;

  private field = "";
  private current = "";
  private parent = "";
  private listed: DirectoryEntry[] = [];
  private filterText = "";
  private hiddenShown = false;
  private statusText = "";
  private selection: string | undefined;
  private inlineName: string | undefined;
  private promptState: { parent: string; name: string } | undefined;
  private request = 0;
  private createRequest = 0;
  private ticket = 0;
  private creatingNow = false;
  private createdPath = "";
  private creationError = "";
  private listEnabled = false;
  private chooseEnabled = false;
  private chooseText = tr(USE_THIS);
  private newFolderEnabled = false;
  private dialogEnabled = false;
  private closed = false;

  constructor(private readonly calls: FolderBrowserCalls, private readonly computer: string, start: string) {
    this.navigate(start === "" ? "~" : start);
  }

  view(): FolderBrowserView {
    const prompt = this.promptState;
    return {
      title: tr("Choose folder — %1", this.computer),
      machine: tr("Folders on %1", this.computer),
      path: this.field, current: this.current, entries: this.visible(), filter: this.filterText, hidden: this.hiddenShown,
      status: this.statusText, selected: this.selection,
      inline: this.inlineName === undefined ? undefined : { name: this.inlineName, createEnabled: validFolderName(this.inlineName, this.current) },
      prompt: prompt && {
        title: tr("New folder — %1", this.computer), text: tr("Create a folder in %1 on %2.", prompt.parent, this.computer),
        name: prompt.name, createEnabled: validFolderName(prompt.name, prompt.parent),
      },
      controlsEnabled: !this.creatingNow, listEnabled: this.listEnabled, newFolderEnabled: this.newFolderEnabled,
      dialogEnabled: this.dialogEnabled, chooseEnabled: this.chooseEnabled, chooseText: this.chooseText, creating: this.creatingNow,
    };
  }

  /** Closing is refused while a folder is being created (195-198). */
  canClose() { return !this.creatingNow; }

  /** The folder Use this folder or Use selected folder chooses (113-118). */
  chosen(): string { return this.selection ?? this.current; }

  /** The browser closed: later answers change nothing. */
  dispose() { this.closed = true; }

  // Navigation (186-193 and the buttons, 87-89, 119).

  navigate(path: string) {
    if (this.creatingNow) return;
    this.cancelInline(false);
    this.creationError = "";
    this.field = path;
    this.statusText = tr(LOADING);
    this.chooseEnabled = false;
    this.listEnabled = false;
    this.newFolderEnabled = false;
    this.dialogEnabled = false;
    const request = ++this.ticket;
    this.request = request;
    this.calls.list(path, this.hiddenShown).then((data) => this.answered(request, object(data)), (failure) => this.failed(request, errorText(failure)));
    this.changed();
  }

  up() { this.navigate(this.parent === "" ? "~" : this.parent); }
  home() { this.navigate("~"); }
  go() { this.navigate(this.field); }

  /** The path field edited by hand (108-112): a listing on its way is dropped. */
  editPath(text: string) {
    if (this.creatingNow) return;
    this.cancelInline(false);
    this.request = 0;
    this.creationError = "";
    this.field = text;
    this.newFolderEnabled = true;
    this.dialogEnabled = true;
    this.listEnabled = false;
    this.chooseEnabled = false;
    this.statusText = tr(TYPED);
    this.changed();
  }

  setHidden(on: boolean) {
    if (this.creatingNow || on === this.hiddenShown) return;
    this.hiddenShown = on;
    this.navigate(this.current === "" ? "~" : this.current);
  }

  /** The filter hides the folders whose name does not hold it; a hidden selection is dropped (120-126). */
  setFilter(text: string) {
    if (this.creatingNow) return;
    this.filterText = text;
    if (this.selection !== undefined && !this.visible().some((e) => e.path === this.selection)) this.select(undefined);
    this.changed();
  }

  /** The list's current folder (104-107). */
  select(path: string | undefined) {
    this.selection = path;
    if (this.current !== "") this.chooseText = tr(path !== undefined ? USE_SELECTED : USE_THIS);
    this.changed();
  }

  /** A double-click, or Enter on the list (101-103, 175-181). */
  open(path: string) {
    if (!this.listEnabled) return;
    this.navigate(path);
  }

  // New folder (208-326).

  newFolder() {
    if (this.creatingNow || this.request) return;
    if (this.current === "" || this.field !== this.current) { this.newFolderDialog(); return; }
    if (this.inlineName !== undefined) return;
    this.filterText = "";
    // The new row is the list's current one.
    this.selection = undefined;
    this.chooseText = tr(USE_THIS);
    this.inlineName = "";
    this.chooseEnabled = false;
    this.changed();
  }

  setInlineName(name: string) {
    if (this.inlineName === undefined) return;
    this.inlineName = name;
    this.changed();
  }

  createInline() {
    const name = this.inlineName;
    if (name === undefined || !validFolderName(name, this.current)) return;
    this.createFolder(childPath(this.current, name));
  }

  /** Cancel new folder, and Escape in its name (173, 274-284). */
  cancelInline(notify = true) {
    if (this.inlineName === undefined) return;
    this.inlineName = undefined;
    this.selection = undefined;
    this.chooseText = tr(USE_THIS);
    this.chooseEnabled = !this.creatingNow && !this.request && this.current !== "" && this.field === this.current;
    if (notify) this.changed();
  }

  /** New folder dialog… (286-318): it opens on the typed path's parent, with its last name. */
  newFolderDialog() {
    if (this.creatingNow || this.request) return;
    const typed = this.field;
    const current = this.current !== "" && typed === this.current;
    const split = typed.lastIndexOf("/");
    const parent = current ? this.current : split === 0 ? "/" : split < 0 ? typed : typed.slice(0, split);
    if (parent !== "~" && !parent.startsWith("/") && !parent.startsWith("~/")) return;
    const name = this.inlineName ?? (current ? "" : typed.slice(split + 1));
    this.cancelInline(false);
    this.promptState = { parent, name };
    this.changed();
  }

  setPromptName(name: string) {
    if (!this.promptState) return;
    this.promptState = { ...this.promptState, name };
    this.changed();
  }

  confirmPrompt() {
    const prompt = this.promptState;
    if (!prompt || !validFolderName(prompt.name, prompt.parent)) return;
    this.promptState = undefined;
    this.createFolder(childPath(prompt.parent, prompt.name));
  }

  cancelPrompt() {
    if (!this.promptState) return;
    this.promptState = undefined;
    this.changed();
  }

  // Answers.

  private visible(): DirectoryEntry[] {
    const query = this.filterText.toLowerCase();
    return query === "" ? this.listed : this.listed.filter((entry) => entry.name.toLowerCase().includes(query));
  }

  private answered(request: number, data: Json) {
    if (this.closed || request !== this.request || this.creatingNow) return;
    this.request = 0;
    this.cancelInline(false);
    this.current = string(data.path);
    this.parent = string(data.parent);
    this.field = this.current;
    this.filterText = "";
    this.selection = undefined;
    this.listed = array(data.directories).slice(0, MAX_ENTRIES).map((value) => {
      const folder = object(value);
      return { name: string(folder.name), path: string(folder.path) };
    });
    this.listEnabled = true;
    this.chooseEnabled = true;
    this.newFolderEnabled = true;
    this.dialogEnabled = true;
    this.chooseText = tr(USE_THIS);
    this.statusText = this.creationError !== "" ? tr(AVAILABLE)
      : this.current === this.createdPath ? tr(CREATED)
        : data.truncated === true ? tr(TRUNCATED)
          : this.listed.length ? LISTED : EMPTY;
    this.changed();
  }

  private failed(request: number, error: string) {
    if (this.closed || request !== this.request || this.creatingNow) return;
    this.request = 0;
    this.statusText = this.creationError === "" ? error : this.creationError;
    this.current = "";
    this.listed = [];
    this.selection = undefined;
    this.listEnabled = true;
    this.chooseEnabled = false;
    this.newFolderEnabled = true;
    this.dialogEnabled = true;
    this.changed();
  }

  private setCreating(creating: boolean) {
    this.creatingNow = creating;
    this.listEnabled = !creating;
    this.newFolderEnabled = !creating;
    this.dialogEnabled = !creating;
    this.chooseEnabled = !creating && this.current !== "";
  }

  private createFolder(path: string) {
    if (this.creatingNow) return;
    this.cancelInline(false);
    this.request = 0;
    this.createdPath = "";
    this.creationError = "";
    this.field = path;
    this.statusText = tr("Creating folder on %1…", this.computer);
    this.setCreating(true);
    const request = ++this.ticket;
    this.createRequest = request;
    this.calls.create(path).then((data) => this.created(request, object(data)), (failure) => this.creationFailed(request, errorText(failure)));
    this.changed();
  }

  private created(request: number, data: Json) {
    if (this.closed || request !== this.createRequest || !this.creatingNow) return;
    this.setCreating(false);
    this.createdPath = string(data.path);
    this.navigate(this.createdPath);
  }

  /** The folder may exist although its receipt was lost: the typed path is read again, never created again. */
  private creationFailed(request: number, error: string) {
    if (this.closed || request !== this.createRequest || !this.creatingNow) return;
    this.setCreating(false);
    this.current = "";
    this.listed = [];
    this.selection = undefined;
    this.chooseEnabled = false;
    this.navigate(this.field);
    this.creationError = error;
  }

  private changed() { if (!this.closed) this.onChange?.(); }
}
