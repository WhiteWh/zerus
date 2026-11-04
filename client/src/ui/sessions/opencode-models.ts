// OpenCode models and providers as hgs reports them (account row
// `model_options`, `hgs account models`), ported from tray/src/OpenCodeModels.cpp,
// OpenCodeEffort.h, the New session dialog's model rows (NewSessionDialog.cpp
// 51-105) and the list rules of OpenCodeModelPicker.cpp. Pinned by the shared
// tests/fixtures/opencode-provider-catalog.json and opencode-effort-ids.json.
// Everything here is display data from another computer: entries are
// validated and bounded, never trusted. Qt counts text in UTF-16 units, as
// JavaScript strings do.

import { array, object, string, type Json } from "../lib/json";
import { tr, trNoop } from "../../i18n";

export interface Model {
  value: string; label: string; provider: string; providerLabel: string;
  free: boolean; favorite: boolean; isDefault: boolean;
  efforts: string[]; effortLabels: string[];
}
export interface Provider { id: string; label: string; kind: string; activeLabel: string; credentials: number; models: number; folder: boolean }
export interface Group { provider: string; label: string; folder: boolean; models: Model[] }

/** QJsonValue::toString(fallback): a string, else the fallback. */
const stringOr = (value: unknown, fallback: string) => (typeof value === "string" ? value : fallback);
/** QJsonValue::toInt(fallback): an integral number, else the fallback. */
const intOr = (value: unknown, fallback: number) => (typeof value === "number" && Number.isInteger(value) ? value : fallback);
const utf8Length = (text: string) => new TextEncoder().encode(text).length;

/** QChar::Other_Control. */
export const controlText = (text: string) => /\p{Cc}/u.test(text);
/** QChar::isSpace: the separators and the controls 9-13, 0x85 and 0xA0. */
const space = (c: string) => {
  const code = c.codePointAt(0) ?? 0;
  return code === 0x20 || (code >= 0x09 && code <= 0x0d) || code === 0x85 || code === 0xa0 || /\p{Z}/u.test(c);
};

/** Catalog text is shown only as a short line: longer, with a control character or of another type, it reads as empty. */
function shortText(value: unknown, limit: number): string {
  const text = string(value);
  return text.length > limit || controlText(text) ? "" : text;
}

/** Provider IDs as hgs accepts them: at most 200 UTF-8 bytes, no '/', whitespace or control character. */
const validProvider = (id: string) => id !== "" && utf8Length(id) <= 200 && !id.includes("/") && !Array.from(id).some((c) => space(c) || controlText(c));

/** OpenCodeEffort::valid: non-empty, at most 40 UTF-8 bytes, no whitespace, no control character and no '/'. */
export const effortValid = (id: string) => id !== "" && utf8Length(id) <= 40 && !id.includes("/") && !Array.from(id).some((c) => space(c) || controlText(c));

/** Non-empty, at most 256 UTF-8 bytes, contains '/', no control characters. */
export const validValue = (value: string) => value !== "" && utf8Length(value) <= 256 && value.includes("/") && !controlText(value);

/** Valid, distinct entries (value, else id), at most `limit`; efforts as `effortValid`, at most 16 of the first 64. */
export function parse(entries: unknown, limit: number): Model[] {
  const models: Model[] = [];
  const seen = new Set<string>();
  for (const entry of array(entries)) {
    if (models.length >= limit) break;
    const option = object(entry);
    const value = stringOr(option.value, string(option.id));
    if (!validValue(value) || seen.has(value)) continue;
    seen.add(value);
    const label = shortText(option.label, 160) || value;
    // The provider is the value's prefix unless the entry names a valid one.
    let provider = string(option.provider);
    if (!validProvider(provider)) provider = value.slice(0, value.indexOf("/"));
    const providerLabel = shortText(option.provider_label, 160) || provider;
    const efforts: string[] = [], effortLabels: string[] = [];
    const variants = array(option.effort_options), labels = array(option.effort_labels);
    for (let index = 0; index < Math.min(64, variants.length) && efforts.length < 16; index++) {
      const effort = string(variants[index]);
      if (!effortValid(effort) || efforts.includes(effort)) continue;
      efforts.push(effort);
      effortLabels.push(shortText(labels[index], 80) || effort);
    }
    models.push({ value, label, provider, providerLabel, free: option.free === true, favorite: option.favorite === true, isDefault: option.default === true, efforts, effortLabels });
  }
  return models;
}

/** Valid, distinct provider rows, at most `limit`; a missing kind is empty and missing counts are -1. */
export function providers(entries: unknown, limit = 64): Provider[] {
  const result: Provider[] = [];
  const seen = new Set<string>();
  for (const entry of array(entries)) {
    if (result.length >= limit) break;
    const row = object(entry);
    const id = string(row.id);
    if (!validProvider(id) || seen.has(id)) continue;
    seen.add(id);
    result.push({
      id, label: shortText(row.label, 160) || id, kind: shortText(row.kind, 32), activeLabel: shortText(row.active_label, 160),
      credentials: Math.max(-1, intOr(row.credential_count, -1)), models: Math.max(-1, intOr(row.model_count, -1)), folder: row.folder === true,
    });
  }
  return result;
}

/** Provider groups in order of first appearance; models keep list order. Named by the first model, else the provider row, else the ID. */
export function group(models: Model[], known: Provider[] = []): Group[] {
  const groups: Group[] = [];
  const positions = new Map<string, number>();
  for (const model of models) {
    let position = positions.get(model.provider);
    if (position === undefined) {
      position = groups.length;
      positions.set(model.provider, position);
      const added: Group = { provider: model.provider, label: model.providerLabel, folder: false, models: [] };
      const found = known.find((p) => p.id === model.provider);
      if (found) {
        if (!added.label) added.label = found.label;
        added.folder = found.folder;
      }
      groups.push(added);
    }
    groups[position].models.push(model);
  }
  for (const target of groups) if (!target.label) target.label = target.provider;
  return groups;
}

/** Every whitespace-separated word must match the label, value or provider label (case-insensitive). */
export function filter(models: Model[], query: string): Model[] {
  const words = query.toLowerCase().split(/\s+/u).filter((word) => word !== "");
  if (!words.length) return models;
  return models.filter((model) => {
    const label = model.label.toLowerCase(), value = model.value.toLowerCase(), provider = model.providerLabel.toLowerCase();
    return words.every((word) => label.includes(word) || value.includes(word) || provider.includes(word));
  });
}

const KINDS: Record<string, string> = {
  free: trNoop("Free, no key"), key: trNoop("API key"), oauth: trNoop("Signed in"), env: trNoop("Environment variable"), config: trNoop("Configuration"),
  command: trNoop("Command"), external: trNoop("External credentials"), local: trNoop("Detected locally"),
};
/** "other" and kinds a newer hgs may report read "Connected". */
export const kindText = (kind: string) => (kind in KINDS ? tr(KINDS[kind]) : tr("Connected"));

/** "Default", "Favorite", "Free" in that order, joined by commas (ruling D4; Qt joins them with a dot). */
export function marks(model: Pick<Model, "isDefault" | "favorite" | "free">): string {
  const parts: string[] = [];
  if (model.isDefault) parts.push(tr("Default"));
  if (model.favorite) parts.push(tr("Favorite"));
  if (model.free) parts.push(tr("Free"));
  return parts.join(", ");
}

/** An OpenCode row or session details from an hgs that has `account models` and `account favorite`. */
export const catalogSupported = (rowOrDetails: unknown) => "model_options_truncated" in object(rowOrDetails);

export const updateText = (computer: string) => tr("Update Zerus on %1 to search all models and star favorites.", computer);

// The dialog's model rows (NewSessionDialog.cpp 51-105).

/** One row of the dialog's model list: its value, the text and label shown, and its validated variants. */
export interface ModelOption { value: string; text: string; label: string; efforts: string[]; effortLabels: string[] }

/**
 * `addModelOptions`: valid model entries (value, else id) the list does not
 * have yet, at most 256 per call, each with at most 16 valid and distinct
 * variants of its first 64.
 */
export function addModelOptions(list: ModelOption[], options: unknown): ModelOption[] {
  const out = [...list];
  const entries = array(options);
  for (let index = 0; index < Math.min(256, entries.length); index++) {
    const option = object(entries[index]);
    const id = stringOr(option.value, string(option.id));
    if (!id || utf8Length(id) > 256 || !id.includes("/") || controlText(id) || out.some((row) => row.value === id)) continue;
    let label = string(option.label);
    if (label.length > 160 || controlText(label)) label = "";
    const efforts: string[] = [], effortLabels: string[] = [];
    const variants = array(option.effort_options), labels = array(option.effort_labels);
    for (let variant = 0; variant < Math.min(64, variants.length) && efforts.length < 16; variant++) {
      const effort = string(variants[variant]);
      if (!effortValid(effort) || efforts.includes(effort)) continue;
      let effortLabel = string(labels[variant]);
      if (!effortLabel || effortLabel.length > 80 || controlText(effortLabel)) effortLabel = effort;
      efforts.push(effort);
      effortLabels.push(effortLabel);
    }
    out.push({ value: id, text: !label || label === id ? id : `${label} (${id})`, label: label || id, efforts, effortLabels });
  }
  return out;
}

/** A confirmed favorite change in model entries (value, else id): the mark is set, or removed as hgs leaves it out. */
export function markFavorite(entries: unknown, value: string, favorite: boolean): unknown[] {
  return array(entries).map((entry) => {
    const option = object(entry);
    if (stringOr(option.value, string(option.id)) !== value) return entry;
    const next = { ...option };
    if (favorite) next.favorite = true; else delete next.favorite;
    return next;
  });
}

/** The same change in a ready catalog: both model lists and its favorites, where a new favorite is listed once ({value, available}). */
export function markCatalogFavorite(catalog: Json, value: string, favorite: boolean): Json {
  const next: Json = { ...catalog };
  for (const list of ["model_options", "models"]) if (list in next) next[list] = markFavorite(next[list], value, favorite);
  if (!("favorites" in next)) return next;
  const favorites: unknown[] = [];
  let listed = false;
  for (const entry of array(next.favorites)) {
    const same = string(object(entry).value) === value;
    listed = listed || same;
    if (!favorite && same) continue;
    favorites.push(entry);
  }
  if (favorite && !listed) {
    const available = array(next.models).some((model) => stringOr(object(model).value, string(object(model).id)) === value);
    favorites.push({ value, available });
  }
  next.favorites = favorites;
  return next;
}

// The picker's list (OpenCodeModelPicker.cpp).

// Bounds for data from another computer: hgs keeps at most 256 short-list
// entries, 10,000 catalog models and 64 favorites; the list renders 500 rows.
export const MAX_SHORT_LIST = 256, MAX_CATALOG = 10_000, MAX_FAVORITES = 64, MAX_ROWS = 500;

/** Reasons come from hgs or OpenCode: one bounded line of text. */
export function noticeText(text: string): string {
  let line = "";
  const simplified = Array.from(text).map((c) => (space(c) ? " " : c)).join("").replace(/ +/g, " ").trim();
  for (const c of simplified) {
    if (!controlText(c)) line += c;
    if (line.length > 400) return `${line.slice(0, 399)}…`;
  }
  return line;
}

/** A value no list describes: its provider is the part before the first '/'. */
function unlisted(value: string): Model {
  const provider = value.slice(0, value.indexOf("/"));
  return { value, label: value, provider, providerLabel: provider, free: false, favorite: false, isDefault: false, efforts: [], effortLabels: [] };
}

/** One model row as the picker shows it. */
export interface PickerRow {
  value: string; label: string;
  /** "Unavailable" for an unavailable favorite, else `marks`. */
  marks: string;
  unavailable: boolean; favorite: boolean; pending: boolean; free: boolean; isDefault: boolean;
  /** The row's tooltip: the value, or why an unavailable favorite cannot be chosen. */
  tooltip: string;
  /** The star's tooltip and accessible text. */
  starTooltip: string;
}
export interface PickerGroup {
  /** The header: the provider's label, "{provider} (this folder)" for a folder provider (ruling D4). */
  label: string;
  models: PickerRow[];
}
export interface PickerView {
  groups: PickerGroup[];
  notice: string;
  all: { checked: boolean; enabled: boolean; tooltip: string };
  /** The selected row: the current model when it is listed and can be chosen. */
  selected: string | undefined;
  /** The search text the list follows: the search field keeps it while the popover is closed, as Qt's does. */
  query: string;
  /** Stars can change favorites here; otherwise they are drawn disabled, with the reason as their tooltip. */
  favoritesAvailable: boolean;
}

interface Rendered { provider: string; label: string; folder: boolean; rows: { model: Model; unavailable: boolean; favorite: boolean }[] }

const allTooltip = () => tr("Search every model OpenCode offers");

/**
 * The grouped OpenCode model list with search, marks, a star per model and
 * All models. Owners fetch catalogs and favorites; the picker only asks for
 * them (`onCatalogRequested`, `onFavoriteRequested`). It never starts
 * OpenCode and never changes a favorite itself.
 *
 * EVERY context change (another computer, folder or agent) begins with
 * `setCatalog({})`, which forgets the catalog and a pending request, turns
 * All models off and clears pending stars; a non-empty `setCatalog` for the
 * same context (a refresh) keeps pending stars. The list is drawn at the
 * points Qt redraws it, so `values()` answers as the Qt tree does.
 */
export class ModelPicker {
  onChosen?: (value: string) => void;
  onFavoriteRequested?: (value: string, favorite: boolean) => void;
  onCatalogRequested?: () => void;

  private shortSource: unknown[] | undefined;
  private shortTruncated = false;
  private short: Model[] = [];
  private models: Model[] = [];
  private known: Provider[] = [];
  /** Favorites the ready catalog does not offer. */
  private unavailable: string[] = [];
  private pending = new Set<string>();
  private currentValue = "";
  private failure = "";
  private catalogState = "";
  private catalogReason = "";
  private favoritesReason = "";
  private hasCatalog = false;
  private modelsTruncated = false;
  private loading = false;
  private favoritesAvailable = true;
  private offList = false;
  private all = false;
  private allEnabled = true;
  private allTooltip = allTooltip();
  private query = "";
  private groups: Rendered[] = [];
  private noticeLine = "";

  constructor() { this.render(); }

  setShortList(options: unknown, truncated: boolean) {
    const list = array(options);
    // Owners pass the list on every refresh: an unchanged one keeps the list as drawn.
    if (this.shortSource && this.shortTruncated === truncated && (this.shortSource === list || JSON.stringify(this.shortSource) === JSON.stringify(list))) return;
    this.shortSource = list;
    this.shortTruncated = truncated;
    this.short = parse(list, MAX_SHORT_LIST);
    this.render();
  }

  /** `hgs account models` output; `{}` is the owner's context reset. */
  setCatalog(catalog: Json) {
    this.loading = false;
    this.failure = "";
    this.hasCatalog = Object.keys(catalog).length > 0;
    if (!this.hasCatalog) { this.all = false; this.pending.clear(); }
    this.models = parse(catalog.models, MAX_CATALOG);
    this.known = providers(catalog.providers);
    this.modelsTruncated = catalog.models_truncated === true;
    this.catalogState = string(catalog.catalog_state).slice(0, 32);
    this.catalogReason = noticeText(string(catalog.catalog_reason));
    // `available` is authoritative only in a ready catalog; an entry without it is not unavailable.
    this.unavailable = [];
    if (this.catalogReady()) {
      const favorites = array(catalog.favorites);
      for (let index = 0; index < Math.min(MAX_FAVORITES, favorites.length); index++) {
        const favorite = object(favorites[index]);
        const value = string(favorite.value);
        const available = favorite.available;
        if (typeof available !== "boolean" || available || !validValue(value) || this.unavailable.includes(value)) continue;
        if (!this.models.some((model) => model.value === value)) this.unavailable.push(value);
      }
    }
    this.render();
  }

  setCatalogFailed(reason: string) {
    this.loading = false;
    this.failure = noticeText(reason);
    this.all = false;
    this.render();
  }

  /** An unknown value stays listed. */
  setCurrent(value: string) {
    if (value === this.currentValue) return;
    this.currentValue = value;
    if (this.offList || !this.row(value)) this.render();
  }

  current() { return this.currentValue; }

  /** The rows shown now, in display order. */
  values(): string[] { return this.groups.flatMap((g) => g.rows.map((r) => r.model.value)); }

  model(value: string): Model | undefined {
    // The list shown now answers first; the off-list current model is described by its value.
    const [first, second] = this.showingAll() ? [this.models, this.short] : [this.short, this.models];
    for (const models of [first, second]) {
      const found = models.find((model) => model.value === value);
      // A ready catalog's unavailable favorite stays a favorite, whatever a stale short list says.
      if (found) return { ...found, favorite: found.favorite || this.unavailable.includes(value) };
    }
    if (value === this.currentValue && validValue(value)) return { ...unlisted(value), favorite: this.unavailable.includes(value) };
    return undefined;
  }

  /** As a click on that row: an unavailable favorite, the current one included, is never chosen. */
  select(value: string) {
    const item = this.row(value);
    if (item ? !item.unavailable : !!this.model(value) && !this.unavailable.includes(value)) this.choose(value);
  }

  /** A click on a row (or Enter on it), or on its star. */
  click(value: string, star: boolean) {
    const item = this.row(value);
    if (!item) return;
    if (star) {
      // The owner changes the favorite; a pending change ignores further clicks.
      if (this.favoritesAvailable && !this.pending.has(value)) this.onFavoriteRequested?.(value, !item.favorite);
    } else if (!item.unavailable) this.choose(value);
  }

  setFavoritesAvailable(available: boolean, reason = "") {
    this.favoritesAvailable = available;
    this.favoritesReason = noticeText(reason);
  }

  /** Off: All models is disabled, with `reason` as its tooltip. */
  setAllModelsAvailable(available: boolean, reason = "") {
    this.allEnabled = available;
    this.allTooltip = available ? allTooltip() : noticeText(reason);
    if (!available && this.all) { this.all = false; this.render(); }
  }

  setFavoritePending(value: string, pending: boolean) {
    if (pending) this.pending.add(value); else this.pending.delete(value);
  }

  setFavorite(value: string, favorite: boolean) {
    this.pending.delete(value);
    this.shortSource = undefined;
    for (const models of [this.short, this.models]) for (const model of models) if (model.value === value) model.favorite = favorite;
    if (!favorite) this.unavailable = this.unavailable.filter((v) => v !== value);
    const item = this.row(value);
    if (!item) return;
    // Stars change in place, so the list keeps its scroll position.
    if (!favorite && item.unavailable) {
      // A row that stays listed (the current model, a stale short-list entry) is drawn again without the mark.
      if (this.model(value)) { this.render(); return; }
      for (const g of this.groups) g.rows = g.rows.filter((r) => r.model.value !== value);
      this.groups = this.groups.filter((g) => g.rows.length);
      return;
    }
    item.favorite = favorite;
  }

  showingAll() { return this.all; }

  /** A click on All models: the first asks for the catalog; later clicks reuse a ready one. */
  toggleAll() {
    if (!this.allEnabled) return;
    this.all = !this.all;
    this.failure = "";
    if (this.all && !this.catalogReady() && !this.loading) {
      this.loading = true;
      this.render();
      this.onCatalogRequested?.();
      return;
    }
    this.render();
  }

  /** The search text (the view waits for a pause in typing before it calls this). */
  setQuery(text: string) {
    this.query = text.slice(0, 200);
    this.render();
  }

  view(): PickerView {
    const groups = this.groups.map((g) => ({
      label: g.folder ? `${g.label} (this folder)` : g.label,
      models: g.rows.map(({ model, unavailable, favorite }) => ({
        value: model.value, label: model.label,
        marks: unavailable ? tr("Unavailable") : marks({ isDefault: model.isDefault, favorite, free: model.free }),
        unavailable, favorite, pending: this.pending.has(model.value), free: model.free, isDefault: model.isDefault,
        tooltip: unavailable ? tr("OpenCode does not offer %1 here. Its star removes the favorite.", model.value) : model.value,
        starTooltip: this.favoritesAvailable ? (favorite ? tr("Remove from favorites") : tr("Add to favorites")) : this.favoritesReason,
      })),
    }));
    const current = this.row(this.currentValue);
    return {
      groups, notice: this.noticeLine,
      all: { checked: this.all, enabled: this.allEnabled, tooltip: this.allTooltip },
      selected: current && !current.unavailable ? this.currentValue : undefined,
      query: this.query,
      favoritesAvailable: this.favoritesAvailable,
    };
  }

  private catalogReady() { return this.hasCatalog && this.catalogState === "ready"; }

  private row(value: string) {
    if (!value) return undefined;
    for (const g of this.groups) for (const r of g.rows) if (r.model.value === value) return r;
    return undefined;
  }

  private choose(value: string) {
    // Every route (click, Enter, select) ends here: a ready catalog's unavailable favorite is never chosen.
    if (this.unavailable.includes(value)) return;
    const rebuild = this.offList || !this.row(value);
    this.currentValue = value;
    if (rebuild) this.render();
    this.onChosen?.(value);
  }

  private render() {
    const query = this.query;
    // All models lists the catalog once it has one to show; until then the short list stays.
    const all = this.showingAll() && this.hasCatalog && (this.catalogReady() || this.models.length > 0);
    const listed = all ? this.models : this.short;
    this.offList = validValue(this.currentValue) && !listed.some((model) => model.value === this.currentValue);
    const current = this.offList ? filter([this.model(this.currentValue)!], query) : [];
    let matches = filter(listed, query);
    let gone: Model[] = [];
    if (all && this.catalogReady()) for (const value of this.unavailable) if (!this.offList || value !== this.currentValue) gone.push(unlisted(value));
    gone = filter(gone, query);
    // One budget of MAX_ROWS model rows in display order: the off-list current
    // model, the list, then unavailable favorites. A listed current model past
    // the cut replaces the last kept row, so it stays visible in its group.
    const total = current.length + matches.length + gone.length;
    let budget = MAX_ROWS - current.length;
    if (matches.length > budget) {
      const at = matches.findIndex((model) => model.value === this.currentValue);
      const kept = matches.slice(0, budget);
      if (at >= budget) { kept.pop(); kept.push(matches[at]); }
      matches = kept;
    }
    budget -= matches.length;
    if (gone.length > budget) gone = gone.slice(0, budget);
    const hidden = total - current.length - matches.length - gone.length;
    const groups: Rendered[] = [];
    const add = (provider: string, label: string, folder: boolean, models: Model[], unavailable: boolean) => {
      // A ready catalog's unavailable favorite is marked in every group: starred, never chosen.
      groups.push({ provider, label, folder, rows: models.map((model) => {
        const out = unavailable || this.unavailable.includes(model.value);
        return { model, unavailable: out, favorite: out || model.favorite };
      }) });
    };
    // The current model is listed first, in its own group, even when this list does not offer it.
    if (current.length) add(current[0].provider, current[0].providerLabel, false, current, false);
    for (const g of group(matches, this.known)) add(g.provider, g.label, g.folder, g.models, false);
    if (gone.length) add("", tr("Unavailable favorites"), false, gone, true);
    this.groups = groups;
    const reason = this.hasCatalog && !this.catalogReady() && this.catalogReason !== "";
    let notice = "";
    if (this.showingAll() && this.loading) notice = tr("Loading all models…");
    else if (!this.showingAll() && this.failure) notice = this.failure;
    else if (reason) notice = this.catalogReason;
    else if (hidden) notice = tr("Showing the first %1 matches. Search to narrow the list.", MAX_ROWS);
    else if (all && this.modelsTruncated) notice = tr("Showing the first %1 models. Search to narrow the list.", this.models.length);
    else if (!all && this.shortTruncated) notice = tr("Some models are not in this short list. Use All models to search every model.");
    this.noticeLine = notice;
  }
}
