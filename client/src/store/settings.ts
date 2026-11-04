import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import defaults from "./settings-defaults.json";

// The client's local preferences (Settings, phase 3b), under Qt's QSettings
// names with Qt's defaults. In the app they live in the core's store
// (<data_dir>/settings.json): read with `settings_get`, written with
// `settings_set`, which answers with the value in effect (clamped as Qt
// clamps it) and the store's revision, and announced as `settings-changed`
// with the same revision. The defaults come from the core's key table
// (settings-defaults.json, kept equal by a core test). Outside Tauri (the
// browser dev mode) they fall back to local storage.
//
// Writes and events can arrive out of call order, so each key has at most
// one save in flight, later values coalesce to the newest, and an answer or
// event older than what the key already shows is dropped.
//
// A measurement run may try a value without storing it (`persist: false`);
// `restoreSettings` returns to what the user chose.

export type SettingValue = boolean | number | string | string[];
export type Settings = Record<string, SettingValue>;

/** Every setting's default, by its QSettings name. */
export const DEFAULTS: Readonly<Settings> = defaults;

/** Local storage of the browser dev mode (and of builds before the core store). */
export const STORAGE_KEY = "zerus-client-settings";
/** Overview's and Activity's column widths before the core store. */
export const OVERVIEW_WIDTH_KEY = "zerus-client-overview-width";
export const ACTIVITY_WIDTH_KEY = "zerus-client-activity-width";
export const SESSIONS_WIDTH_KEY = "zerus-client-sessions-width";
export const PROJECT_SPLIT_KEY = "zerus.projectFolderSplit";
/** This viewer's copy of what the first paint needs (settings.json stays the truth). */
export const HINT_KEY = "zerus-client-settings-hint";
export const HINTED = ["workspace/theme", "client/darkSkin", "client/lightSkin", "workspace/contentScale"] as const;

export interface SettingsStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** A store answer: the value in effect (null for a removed override) and the revision. */
export interface Saved { value: SettingValue | null; revision: number }

/** Where settings live: the core store in the app, local storage outside it. */
export interface SettingsBackend {
  /** Every setting as the store has it, and the store's revision. */
  load(): Promise<{ values: Settings; revision: number }>;
  /** Stores one setting. */
  save(key: string, value: SettingValue | null): Promise<Saved>;
  /** Changes made elsewhere; resolves to the unregister function. */
  listen(onChange: (key: string, change: Saved) => void): Promise<() => void>;
}

/** The stored object, or an empty one when it is missing or not a JSON object. */
function storedObject(raw: string | null | undefined): Record<string, unknown> {
  if (!raw) return {};
  try {
    const value: unknown = JSON.parse(raw);
    return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

/** A value of the same type as the setting's default, or undefined. */
export function sameType(key: string, value: unknown): SettingValue | undefined {
  if (key === "workspace/inspectorOpen") return typeof value === "boolean" || value === "auto" ? value : undefined;
  const fallback = DEFAULTS[key];
  if (fallback === undefined || value === null || value === undefined) return undefined;
  if (Array.isArray(fallback)) return Array.isArray(value) && value.every((item) => typeof item === "string") ? value as string[] : undefined;
  return typeof value === typeof fallback ? value as SettingValue : undefined;
}

/** A shortcut override: a list of portable chords under shortcuts/<id>. */
function shortcut(key: string, value: unknown): string[] | undefined {
  return key.startsWith("shortcuts/") && Array.isArray(value) && value.every((item) => typeof item === "string") ? value as string[] : undefined;
}

function machinePreference(key: string, value: unknown): SettingValue | undefined {
  const match = /^machines\/(colors|vivid)\/([^/\\\u0000-\u001f\u007f]+)$/u.exec(key);
  if (!match || new TextEncoder().encode(match[2]).length > 256) return undefined;
  if (match[1] === "vivid") return typeof value === "boolean" ? value : undefined;
  return typeof value === "string" && /^#(?:[\da-f]{3}|[\da-f]{6})$/iu.test(value) ? value : undefined;
}

function projectPreference(key: string, value: unknown): boolean | undefined {
  const prefix = "projects/vivid/";
  if (!key.startsWith(prefix)) return undefined;
  const id = key.slice(prefix.length);
  return id && new TextEncoder().encode(id).length <= 256 && !/[\u0000-\u001f\u007f-\u009f]/u.test(id) && typeof value === "boolean" ? value : undefined;
}
const typed = (key: string, value: unknown) => sameType(key, value) ?? shortcut(key, value) ?? machinePreference(key, value) ?? projectPreference(key, value);

function typedObject(raw: Record<string, unknown>, keys?: readonly string[]): Settings {
  const values: Settings = {};
  for (const [key, value] of Object.entries(raw)) {
    if (keys && !keys.includes(key)) continue;
    const checked = typed(key, value);
    if (checked !== undefined) values[key] = checked;
  }
  return values;
}

/** The settings of the browser dev mode, kept in local storage. */
export function storageBackend(storage: SettingsStorage | undefined): SettingsBackend {
  const read = () => { try { return storedObject(storage?.getItem(STORAGE_KEY)); } catch { return {}; } };
  let revision = 0;
  return {
    load: async () => ({ values: typedObject(read()), revision }),
    save: async (key, value) => {
      const stored = read();
      if (value === null) delete stored[key];
      else stored[key] = value;
      storage?.setItem(STORAGE_KEY, JSON.stringify(stored));
      return { value: value ?? DEFAULTS[key] ?? null, revision: ++revision };
    },
    listen: async () => () => {},
  };
}

/** Values to move once from local storage, and how to remove them after. */
export interface Legacy { moves: Settings; clear(): void }

/**
 * What local storage held before the core store: the settings object (in the
 * app only; the browser dev mode keeps using it) and the Overview and
 * Activity widths. `clear` removes them; call it only after every move saved.
 */
export function takeLegacy(storage: SettingsStorage | undefined, withSettings: boolean): Legacy {
  const moves: Settings = {};
  try {
    if (withSettings) Object.assign(moves, typedObject(storedObject(storage?.getItem(STORAGE_KEY))));
    const width = Number(storage?.getItem(OVERVIEW_WIDTH_KEY));
    if (Number.isFinite(width) && width > 0) moves["client/overviewWidth"] = Math.round(width);
    const column = Number(storage?.getItem(ACTIVITY_WIDTH_KEY));
    if (Number.isFinite(column) && column > 0) moves["workspace/activityWidth"] = Math.round(column);
    for (const [old, key, min, max] of [[SESSIONS_WIDTH_KEY, "workspace/sessionsWidth", 270, 2400], [PROJECT_SPLIT_KEY, "workspace/projectFolderSplit", 160, 4096]] as const) {
      const value = Number(storage?.getItem(old));
      if (Number.isFinite(value) && value >= min) moves[key] = Math.round(Math.min(max, value));
    }
  } catch {
    /* storage may be unavailable */
  }
  return {
    moves,
    clear: () => {
      try {
        if (withSettings) storage?.removeItem(STORAGE_KEY);
        storage?.removeItem(OVERVIEW_WIDTH_KEY);
        storage?.removeItem(ACTIVITY_WIDTH_KEY);
        storage?.removeItem(SESSIONS_WIDTH_KEY);
        storage?.removeItem(PROJECT_SPLIT_KEY);
      } catch {
        /* storage may be unavailable */
      }
    },
  };
}

/** The first-paint copy (theme, skins, content scale) this viewer kept. */
export function readHint(storage: SettingsStorage | undefined): Settings {
  try { return typedObject(storedObject(storage?.getItem(HINT_KEY)), HINTED); } catch { return {}; }
}

export function writeHint(storage: SettingsStorage | undefined, settings: Settings) {
  const hint: Settings = {};
  for (const key of HINTED) if (settings[key] !== undefined) hint[key] = settings[key];
  try { storage?.setItem(HINT_KEY, JSON.stringify(hint)); } catch { /* storage may be unavailable or full */ }
}

interface Slot { queued?: { value: SettingValue | null }; running: boolean; waiters: ((ok: boolean) => void)[] }

/**
 * A reactive settings store over `backend`, starting from the defaults (and
 * this viewer's hint) until the backend's values arrive. The legacy moves are
 * written first and cleared only once all saved; `remember` keeps the hint,
 * from confirmed values only and once the backend's values have loaded.
 */
export function createSettings(backend: SettingsBackend, options: { legacy?: Legacy; hint?: Settings; remember?: (settings: Settings) => void } = {}) {
  const [settings, setSettings] = createStore<Settings>({ ...DEFAULTS, ...options.hint });
  const [loaded, setLoaded] = createSignal(false);
  // What the store last confirmed, the newest revision applied per key, the
  // keys holding a value tried without storing, and the saves per key.
  const chosen: Settings = { ...DEFAULTS };
  const revisions = new Map<string, number>();
  const tried = new Set<string>();
  const slots = new Map<string, Slot>();

  const show = (key: string, value: SettingValue | null | undefined) => {
    // Undefined removes the key: a shortcut's defaults apply again.
    if (!tried.has(key)) setSettings(key, (value ?? undefined) as SettingValue);
  };
  /** Whether a change of `key` at `revision` is the newest seen. */
  const fresh = (key: string, revision: number) => {
    if (revision <= (revisions.get(key) ?? 0)) return false;
    revisions.set(key, revision);
    return true;
  };
  const confirm = (key: string, value: SettingValue | null) => {
    if (value === null) delete chosen[key];
    else chosen[key] = value;
  };
  // Whether the backend's values loaded: until then (or after a failed
  // load) the confirmed values are partly defaults, and the hint stays.
  let complete = false;
  /**
   * Keeps this viewer's first-paint copy after a hinted key changed: what the
   * store confirmed, never a value still being saved.
   */
  const remember = (key: string) => {
    if (complete && (HINTED as readonly string[]).includes(key)) options.remember?.({ ...chosen });
  };

  async function drain(key: string, slot: Slot) {
    slot.running = true;
    while (slot.queued) {
      const { value } = slot.queued;
      slot.queued = undefined;
      const waiters = slot.waiters;
      slot.waiters = [];
      let ok = true;
      let saved: Saved | undefined;
      try {
        saved = await backend.save(key, value);
      } catch (error) {
        ok = false;
        console.warn(`settings: ${key} not saved: ${String(error)}`);
      }
      if (saved && fresh(key, saved.revision)) confirm(key, saved.value);
      // Once the newest value has its answer, the key shows what the store
      // holds: this answer, or the event for it (the core announces a change
      // before it answers), a newer change from elsewhere, or after a
      // refusal the last confirmed value.
      if (!slot.queued) {
        show(key, chosen[key]);
        if (ok) remember(key);
      }
      for (const waiter of waiters) waiter(ok);
    }
    slot.running = false;
    slots.delete(key);
  }

  /** Stores one setting; whether the store took it. */
  function set(key: string, value: SettingValue | null, options: { persist?: boolean } = {}): Promise<boolean> {
    if (options.persist === false) {
      if (value === null) return Promise.resolve(true);
      tried.add(key);
      setSettings(key, value);
      return Promise.resolve(true);
    }
    tried.delete(key);
    show(key, value);
    return new Promise((resolve) => {
      let slot = slots.get(key);
      if (!slot) { slot = { running: false, waiters: [] }; slots.set(key, slot); }
      slot.queued = { value };
      slot.waiters.push(resolve);
      if (!slot.running) void drain(key, slot);
    });
  }

  /** Back to what the user chose, after values tried with `persist: false`. */
  function restore() {
    const keys = [...tried];
    tried.clear();
    for (const key of keys) show(key, chosen[key]);
  }

  /** A change made elsewhere: shown unless one of ours is still being saved. */
  const changed = (key: string, change: Saved) => {
    if (!fresh(key, change.revision)) return;
    confirm(key, change.value);
    if (!slots.has(key)) { show(key, change.value); remember(key); }
  };

  let unlisten: (() => void) | undefined;
  const ready = (async () => {
    try {
      unlisten = await backend.listen(changed);
    } catch (error) {
      console.warn(`settings: changes made elsewhere will not show: ${String(error)}`);
    }
    const moves = options.legacy?.moves ?? {};
    const results = await Promise.all(Object.entries(moves).map(([key, value]) => set(key, value)));
    if (results.every(Boolean)) options.legacy?.clear();
    try {
      const { values, revision } = await backend.load();
      for (const [key, value] of Object.entries(values)) {
        // A change answered at a newer revision is newer than the loaded
        // value; an equal one is in it (a fresh store loads at revision 0).
        // Events older than the load can no longer show.
        if (revision < (revisions.get(key) ?? 0)) continue;
        revisions.set(key, revision);
        confirm(key, value);
        // A save in flight keeps showing its value until it is answered.
        if (!slots.has(key)) show(key, value);
      }
      complete = true;
      options.remember?.({ ...chosen });
    } catch (error) {
      console.warn(`settings: not loaded: ${String(error)}`);
    }
    setLoaded(true);
  })();

  return { settings, set, restore, loaded, ready, dispose: () => unlisten?.() };
}

function browserStorage(): SettingsStorage | undefined {
  try {
    return typeof window === "undefined" ? undefined : window.localStorage;
  } catch {
    return undefined; // storage can be disabled
  }
}

const inTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** The core store through the Tauri commands. */
function tauriBackend(): SettingsBackend {
  return {
    load: () => invoke<{ values: Settings; revision: number }>("settings_get"),
    save: (key, value) => invoke<Saved>("settings_set", { key, value }),
    listen: (onChange) => listen<{ key: string; value: SettingValue | null; revision: number }>("settings-changed",
      (e) => onChange(e.payload.key, { value: e.payload.value, revision: e.payload.revision })),
  };
}

const storage = browserStorage();
const store = inTauri()
  ? createSettings(tauriBackend(), { legacy: takeLegacy(storage, true), hint: readHint(storage), remember: (s) => writeHint(storage, s) })
  : createSettings(storageBackend(storage), { legacy: takeLegacy(storage, false) });

/** The client's settings by QSettings name, reactive. */
export const settings = store.settings;
/** Changes one setting and stores it (unless `persist: false`); `null` removes a supported override; whether it was stored. */
export const setSetting = store.set;
/** Undoes values tried with `persist: false`. */
export const restoreSettings = store.restore;
/** Whether the stored values have arrived. */
export const settingsLoaded = store.loaded;
/** Resolves once the stored values have arrived. */
export const settingsReady = store.ready;

export const flag = (key: string): boolean => settings[key] === true;
export const num = (key: string): number => { const value = settings[key]; return typeof value === "number" ? value : Number(DEFAULTS[key] ?? 0); };
export const text = (key: string): string => { const value = settings[key]; return typeof value === "string" ? value : String(DEFAULTS[key] ?? ""); };
