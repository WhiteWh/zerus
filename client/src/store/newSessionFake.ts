import { machineMeta, NOW, usageOf } from "./testing";
import { createNewSessionStore, type NewSessionBridge, type NewSessionDeps, type NewSessionOptions } from "./newSession";
import type {
  Account, CreationAdmission, Details, FolderOut, HeaderOut, LaunchAccounts, LaunchForm, LaunchShown, MachineMeta, NewSessionContext, NewSessionFormInput, Profile,
  ProjectedWindow, RepositoryFields,
} from "../types";
import type { Session } from "../ui/sessions/types";

// The fake world of the New session store tests (newSession.test.ts,
// newSessionOpencode.test.ts): a recorded bridge that keeps each command the
// store sends, answering as tray/tests/test_newsessiondialog.cpp's fake hgs
// does. Nothing here runs hgs. Synthetic computers and paths only. Each test
// file mocks "./ui" and "../bridge" itself (vi.mock is per file).

export type Json = Record<string, unknown>;
export interface Call { command: string; args: unknown[] }

/** Commands that create or change something; opening, choosing and reading send none of them. */
export const CREATING = ["creationSubmit", "taskExecutorSubmit", "createWorktree", "createDirectory", "openCodeStart", "openCodeFavorite"];

export const NOT_LOADED: LaunchAccounts = { loaded: false, error: null, profiles: [], groups: [] };

export const settle = async () => { for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0)); };

/** A project of the fake organization: its header and the folders `project_folders` answers. */
export type FakeProject = HeaderOut & { folders: FolderOut[] };

export function deferred() {
  let release!: () => void;
  const done = new Promise<void>((resolve) => { release = resolve; });
  return { done, release };
}

// The fake world: the Qt fixture's accounts, catalogs and OpenCode service.

export const window = (used: number, minutes: number, resets: number): ProjectedWindow => ({ id: `w${minutes}`, label: "", used, minutes, resets_at: resets });

export function member(host: string, id: string, provider: string, label: string, extra: Partial<Profile> = {}): Profile {
  return {
    key: `${host}:${provider}:${id}`, host, machine: host || "pc", id, label, provider, native: id.startsWith("native-"), installed: true,
    is_default: false, home: `/profiles/${id}`, auth_revision: "", online: true, state: "ready", detail: { source: "" }, signed_in: true,
    permission: "", permission_label: { source: "" }, permission_hint: { source: "" }, model_supported: false, model_text: { source: "" }, usage: usageOf({ provider }), opencode: null, ...extra,
  };
}

export class World {
  flags = new Set<string>();
  favorites: string[] = [];
  calls: Call[] = [];
  gates = new Map<string, ReturnType<typeof deferred>>();
  online: Record<string, boolean> = { "": true, mac: true };
  cards: Record<string, MachineMeta> = { "": machineMeta("pc"), mac: machineMeta("mac") };
  warning: string | null = null;
  /** What `creation_submit` answers; an Error is a lost call (an unknown outcome). */
  admission: CreationAdmission | Error = { admitted: "creation-1" };
  /** Computers whose catalog reply is not filed yet: `launch_accounts` answers `loaded: false`. */
  unfiled = new Set<string>();
  /** Computers hgs cannot read: the core answers with the failure and no rows. */
  unreadable = new Map<string, string>();
  /** Runs inside each `new_session_start`, with its 1-based number. */
  onStart: ((n: number) => void) | undefined;
  localExists = true;
  projects: FakeProject[] = [];
  /** Sessions rows' projects (`groupOf`); a row it does not name is in the default project. */
  groups: Record<string, string> = {};
  swarm = "";
  extraModel: Json | undefined;
  selection: { row: Session; details?: Details } | undefined;
  /** The selected creation row's destination project, if a card is selected. */
  creationProject: string | undefined;
  filters: string[] = [];
  listeners = new Set<(computer: string) => void>();
  ticks: (() => void)[] = [];
  notices: { tone: string; text: string; sticky: boolean }[] = [];
  opened: string[] = [];
  admitted: string[] = [];

  hold(name: string) { const gate = deferred(); this.gates.set(name, gate); return gate; }
  release(name: string) { this.gates.get(name)?.release(); this.gates.delete(name); }
  async wait(name: string) { const gate = this.gates.get(name); if (gate) await gate.done; }
  count(command: string) { return this.calls.filter((c) => c.command === command).length; }
  of(command: string) { return this.calls.filter((c) => c.command === command); }
  creating() { return this.calls.filter((c) => CREATING.includes(c.command)); }

  /** OpenCode's model catalog on an account row: its variants are each model's effort options. */
  rowCatalog(host: string): Json {
    const stopped = this.flags.has("opencode-stopped");
    const many = Array.from({ length: 20 }, (_, n) => `level-${n}`);
    const models: Json[] = [
      { id: "fixture/local", value: "fixture/local", label: "Local fixture", provider: "fixture", provider_label: "fixture", free: true, effort_options: ["low", "medium", "high"], effort_labels: ["low", "medium", "high"] },
      { id: "fixture/plain", value: "fixture/plain", label: "Plain fixture", provider: "fixture", provider_label: "fixture", free: true, effort_options: [], effort_labels: [] },
      { id: "fixture/other", value: "fixture/other", label: "Other fixture", provider: "fixture", provider_label: "fixture", free: false, effort_options: ["high", "max", "", "bad value", "x".repeat(80), "max"], effort_labels: ["High", "Maximum", "", "", "", ""] },
      { id: "fixture/many", value: "fixture/many", label: "Many fixture", provider: "fixture", provider_label: "fixture", free: false, effort_options: many, effort_labels: many },
    ];
    if (this.extraModel) models.push(this.extraModel);
    const result: Json = { model_options: models, settings_model: "fixture/local", catalog_state: stopped ? "not_running" : "ready",
      catalog_reason: stopped ? `OpenCode is not running on ${host || "pc"}` : "", catalog_at: 1791018000 };
    if (!this.flags.has("old-hgs")) result.model_options_truncated = false;
    return result;
  }

  usage(host: string, id: string, provider: string) {
    const ended = this.flags.has("ended");
    const windows = this.flags.has("missing") || provider === "dsh" || provider === "opencode" ? []
      : [window(host === "mac" ? 91 : 17, 300, ended ? NOW - 60 : NOW + 3600), window(78, 10080, ended ? NOW - 60 : NOW + 86_400)];
    const email = provider === "opencode" ? "" : this.flags.has("machine-accounts") ? `${host === "mac" ? "mac" : "pc"}@example.invalid` : "shared@example.invalid";
    return usageOf({ provider, status: provider === "dsh" ? "signed_out" : provider === "opencode" ? "configured" : "ok", identity: { ...usageOf().identity, email }, windows });
  }

  accounts(host: string): LaunchAccounts {
    let rows: [string, string, string, Partial<Profile>][] = [["work", "codex", "Work", {}], ["other", "codex", "Work", { is_default: true }], ["claude", "claude", "Review", {}], ["native-dsh", "dsh", "DeepSeek", {}]];
    if (this.flags.has("removed")) rows = rows.filter(([id]) => id !== "work");
    if (this.flags.has("machine-accounts")) rows = (host === "mac" ? ["mac-claude"] : ["pc-claude", "pc-copy"]).map((id) => [id, "claude", "Review", { is_default: true }]);
    if (this.flags.has("opencode")) rows.push(["native-opencode", "opencode", "Native configuration", { is_default: true }]);
    if (this.flags.has("empty-catalog")) rows = [];
    const profiles: Json[] = rows.map(([id, provider, label, extra]) => ({
      id, provider, label, home: `/profiles/${id}`, installed: true, is_default: !!extra.is_default,
      ...(provider === "opencode" && this.flags.has("opencode-models") ? this.rowCatalog(host) : {}),
    }));
    const members = rows.map(([id, provider, label, extra]) => member(host, id, provider, label, { ...extra, usage: this.usage(host, id, provider) }));
    // machine-accounts: this computer's two rows are one account.
    const grouped: Profile[][] = this.flags.has("machine-accounts") && host !== "mac" ? [members] : members.map((m) => [m]);
    const groups: Account[] = grouped.map((list) => ({
      key: `${list[0].provider}:${list[0].id}`, label: list[0].label, provider: list[0].provider, installed: true, machines: [host || "pc"], members: list,
      usage: list[0].usage, permission: { source: "" }, identity_lines: [], identity_note: null,
    }));
    return { loaded: true, error: this.flags.has("fail") ? "Could not load accounts: exit code 1" : null, profiles, groups };
  }

  folderCatalog(location: string): Json {
    if (this.flags.has("models-stopped")) {
      return { catalog_state: "not_running", catalog_reason: "OpenCode is not running on pc", model_options: [], models: [], providers: [],
        model_options_truncated: false, providers_truncated: false, models_truncated: false, favorites: [] };
    }
    const [tag, name] = location === "/workspace/example" ? ["proj", "Project model"] : ["other", "Other folder model"];
    const short: Json[] = (this.rowCatalog("").model_options as Json[]).filter((m) => location !== "/workspace/nolocal" || m.value !== "fixture/local").map((m) => ({ ...m }));
    short.push({ id: `folderonly/${tag}`, value: `folderonly/${tag}`, label: name, provider: "folderonly", provider_label: "Folder-only fixture", free: false, effort_options: [], effort_labels: [] });
    const full: Json[] = [...short.map((m) => ({ ...m })), { id: "solo/solo-model", value: "solo/solo-model", label: "Solo model", provider: "solo", provider_label: "Solo fixture", free: false, effort_options: ["low"], effort_labels: ["low"] }];
    for (const entry of [...short, ...full]) if (this.favorites.includes(entry.value as string)) entry.favorite = true;
    const held = this.flags.has("hold-next-models") && !this.flags.has("models-held");
    if (held) short.push({ id: "stale/old", value: "stale/old", label: "Stale model", provider: "stale", provider_label: "Stale fixture", free: false, effort_options: [], effort_labels: [] });
    return { catalog_state: "ready", catalog_reason: "", location, model_options: short, models: full,
      providers: [{ id: "folderonly", label: "Folder-only fixture", kind: "config", credential_count: 0, model_count: 1, folder: true }],
      model_options_truncated: false, providers_truncated: false, models_truncated: false, favorites: this.favorites.map((value) => ({ value })) };
  }

  worktrees(path: string): Json {
    if (this.flags.has("prefill-linked")) {
      return { state: "ok", path, selected_root: path, common_dir: "/workspace/example/.git",
        worktrees: [{ path: "/workspace/example", kind: "main", available: true }, { path: "/workspace/example-work-one", kind: "linked", available: true }] };
    }
    return { state: "not_repo", worktrees: [] };
  }
}

/** A minimal `launch::shown` for the fake: Start's enablement, the notice and the preview. */
export function shownOf(f: LaunchForm): LaunchShown {
  const repository = f.repository;
  const automatic = f.auto_preferred && (!!repository || !!f.automatic_available);
  const ready = repository
    ? f.online && repository.can_submit && (!repository.new_project || repository.new_project_name.trim() !== "")
    : !f.prefill_pending && f.path !== "" && f.folder_online && (!f.auto_preferred || !f.catalog_pending);
  const nameProblem = f.name.trim() ? null : { source: "Enter a session name." };
  const source = f.no_machine ? (f.can_enable_local_sessions ? "Enable local sessions for this computer in Machines, or add a remote machine." : "Add a remote machine in Machines to start sessions from this computer.")
    : f.account_preset && !f.account_selected ? (f.account_loading ? "Loading this session's account…" : "This session's account is unavailable. Choose another account.")
      : f.account_count === 0 ? "Restore or add an account in Agent accounts to start this agent."
        : !repository && f.path && !f.folder_online ? "This computer is unavailable. Choose another folder or reconnect it." : null;
  const notice = source === null ? null : { source };
  return {
    start_enabled: !f.no_machine && f.account_selected && (!f.account_loading || f.agent === "sh") && !nameProblem && ready,
    start_text: { source: repository ? "Clone and start" : "Start session" },
    preview: f.prefill_pending ? { source: "Checking the session's main working folder…" } : f.path ? null : { source: "Browse to choose a folder on this computer." },
    outside_project: false, name_problem: nameProblem, notice, automatic, auto_enabled: false, none_enabled: true, worktree_target: "", worktree_tooltip: null,
    choose_existing_enabled: false, create_manually_enabled: false, worktree_options_enabled: false, fields_enabled: true, browse_enabled: true,
    cancel_enabled: true, open_terminal_enabled: true, open_terminal_tooltip: { source: "Also open an external terminal for this session." }, clone_launch_due: false,
  };
}

export function bridgeOf(world: World): NewSessionBridge {
  const record = (command: string, ...args: unknown[]) => { world.calls.push({ command, args }); };
  const context = (): NewSessionContext => ({ branch: "work-1a2b3c4d", own_windows_session: null, build_style: "posix", remembered: { agent: "", accounts: {}, open_terminal: false }, swarm: world.swarm });
  return {
    newSessionContext: async () => { record("newSessionContext"); return world.context ?? context(); },
    newSessionForm: async (input: NewSessionFormInput) => { record("newSessionForm", input); return { shown: shownOf(input.form), automatic_available: false, automatic_path: "", project_worktree: false }; },
    newSessionStart: async (input: NewSessionFormInput, submitted: boolean) => {
      record("newSessionStart", input, submitted);
      const answer = !submitted && shownOf(input.form).start_enabled ? "background" : "nothing";
      world.onStart?.(world.count("newSessionStart"));
      return answer;
    },
    newSessionPlan: async (input, choices) => { record("newSessionPlan", input, choices); return { cmd: choices.agent, tag: input.form.name } as never; },
    launchWarning: async (local, computer) => { record("launchWarning", local, computer); return world.warning; },
    repositoryFields: async (url, destination, style, custom, fresh = null) => {
      record("repositoryFields", url, destination, style, custom, fresh);
      const suggested = (url.split(/[/:]/).pop() ?? "").replace(/\.git$/, "");
      const field = fresh !== null ? `~/${fresh || suggested}` : custom ? destination : `${destination ? destination.slice(0, destination.lastIndexOf("/")) : "~"}/${suggested}`;
      const at = field.lastIndexOf("/");
      const fields: RepositoryFields = { url, parent: at <= 0 ? "/" : field.slice(0, at), name: field.slice(at + 1), field, destination: field, suggested, can_submit: url !== "" && field.slice(at + 1) !== "" };
      return fields;
    },
    worktreeSnapshot: async (computer, path) => { record("worktreeSnapshot", computer, path); return {}; },
    worktreeCatalog: async (computer, path, refresh) => {
      record("worktreeCatalog", computer, path, refresh);
      await world.wait(refresh ? "prefill" : "catalog");
      return world.worktrees(path);
    },
    pickLocalFolder: async (start) => { record("pickLocalFolder", start); return world.picked ?? null; },
    localFolderExists: async (path) => { record("localFolderExists", path); return world.localExists; },
    openCodeModels: async (computer, folder, name) => {
      record("openCodeModels", computer, folder, name);
      if (world.flags.has("old-hgs")) throw `Update Zerus on ${name} to search all models and star favorites.`;
      const held = world.flags.has("hold-next-models") && !world.flags.has("models-held");
      const answer = world.folderCatalog(folder);
      if (held) { world.flags.add("models-held"); await world.wait("next-models"); }
      if (folder === "/workspace/example") await world.wait("models");
      return answer;
    },
    openCodeFavorite: async (computer, value, favorite, name) => {
      record("openCodeFavorite", computer, value, favorite, name);
      await world.wait("favorite");
      if (world.flags.has("favorite-fails")) throw "Account catalog is busy. Retry after the current operation finishes.";
      world.favorites = [...world.favorites.filter((v) => v !== value), ...(favorite ? [value] : [])];
      return { status: "applied", model: value, favorite };
    },
    openCodeStart: async (computer) => { record("openCodeStart", computer); world.flags.delete("opencode-stopped"); return world.rowCatalog(computer); },
    launchAccounts: async (computer, refresh, agent = null) => {
      record("launchAccounts", computer, refresh, agent);
      // The core answers without rows while the reply is not filed; a held answer was built at the call.
      const failure = world.unreadable.get(computer);
      const answer = failure ? { loaded: true, error: failure, profiles: [], groups: [] } : world.unfiled.has(computer) ? NOT_LOADED : world.accounts(computer);
      await world.wait(`accounts:${computer}`);
      return answer;
    },
    launchAccountRows: async (computer) => {
      record("launchAccountRows", computer);
      // The answer is built at the call; a held one arrives late.
      const answer = world.unfiled.has(computer) ? NOT_LOADED : world.accounts(computer);
      await world.wait(`rows:${world.count("launchAccountRows")}`);
      return answer;
    },
    launchAccountsClose: async () => { record("launchAccountsClose"); },
    onLaunchAccountsChanged: (listener) => { world.listeners.add(listener); return () => world.listeners.delete(listener); },
    // The organization's copy answers at once, or after its gate; the list is the folders at the call.
    // `failFolders` names a lost call: the command rejects with its text.
    projectFolders: async (project) => {
      record("projectFolders", project);
      const failure = world.failFolders?.(project, world.count("projectFolders"));
      const listed = world.projects.find((p) => p.id === project)?.folders.map((f) => ({ ...f })) ?? [];
      await world.wait(`folders:${project}`);
      if (failure) throw failure;
      return listed;
    },
    taskExecutorSubmit: async (input) => {
      record("taskExecutorSubmit", input);
      const admission = world.admission;
      await world.wait("submit");
      if (admission instanceof Error) throw admission.message;
      return admission;
    },
    creationSubmit: async (plan) => {
      record("creationSubmit", plan);
      const admission = world.admission;
      await world.wait("submit");
      if (admission instanceof Error) throw admission.message;
      return admission;
    },
  };
}

// Optional parts of the world a test sets.
export interface World {
  context?: NewSessionContext;
  picked?: string;
  /** The text a `project_folders` call rejects with, by project and the call's 1-based number; undefined answers. */
  failFolders?: (project: string, call: number) => string | undefined;
}

const defaultOf = (world: World) => world.projects.find((p) => p.is_default)?.id ?? "ungrouped";

export function setup(configure: (world: World) => void = () => {}) {
  const world = new World();
  world.projects = [project("ungrouped", "Inbox", [], { is_default: true })];
  configure(world);
  const deps: NewSessionDeps = {
    bridge: bridgeOf(world),
    cards: () => world.cards,
    peers: () => ["mac"],
    online: (computer) => world.online[computer] ?? false,
    organization: () => ({ headers: world.projects.map(({ folders: _folders, ...header }) => header), defaultProject: defaultOf(world) }),
    groupOf: (rowId) => world.groups[rowId] ?? defaultOf(world),
    selection: () => world.selection,
    creationProject: () => world.creationProject,
    hostFilters: () => world.filters,
    notify: (tone, text, sticky = false) => world.notices.push({ tone, text, sticky }),
    openProjects: (id) => world.opened.push(`projects:${id}`),
    openMachines: () => world.opened.push("machines"),
    every: (_ms, run) => { world.ticks.push(run); return () => { world.ticks = world.ticks.filter((t) => t !== run); }; },
    now: () => NOW,
  };
  const store = createNewSessionStore(deps);
  store.onCreationAdmitted((id) => world.admitted.push(id));
  const open = async (options: NewSessionOptions = {}) => { await store.openNewSession(options); await settle(); };
  const state = () => { const s = store.newSession(); if (!s) throw new Error("no dialog"); return s; };
  const act = store.actions;
  const row = (id: string) => state().accounts.findIndex((r) => r.id === id && !r.shell);
  const choose = async (id: string) => { act.chooseAccount(row(id)); await settle(); };
  const fire = async (computer: string) => { for (const listener of world.listeners) listener(computer); await settle(); };
  const tick = async () => { for (const run of world.ticks) run(); await settle(); };
  const start = async (answer = true) => { const asked: string[] = []; await store.startNewSession(async (text) => { asked.push(text); return answer; }); await settle(); return asked; };
  const choices = () => world.of("newSessionPlan").at(-1)?.args[1] as Json;
  const values = () => state().picker.groups.flatMap((g) => g.models.map((m) => m.value));
  const star = (value: string) => state().picker.groups.flatMap((g) => g.models).find((m) => m.value === value);
  return { world, store, open, state, act, row, choose, fire, tick, start, choices, values, star };
}

export function project(id: string, name: string, folders: FolderOut[] = [], extra: Partial<FakeProject> = {}): FakeProject {
  return { id, name, color: null, accessible: true, collapsed: false, is_default: false, palette: 0, folders, ...extra };
}
/** A folder as the organization holds it; `id` "" has none, and its key is its computer and path. */
export const folder = (id: string, machine: string, path: string): FolderOut =>
  ({ id, key: id || `${machine}\n${path}`, machine, path, name: "", machine_id: "", machine_name: "" });

