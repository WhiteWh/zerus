import { createSignal } from "solid-js";
import * as bridge from "../bridge";
import type { Outcome, Target } from "../types";
import type { Session } from "../ui/sessions/types";
import { ModelPicker, catalogSupported, markCatalogFavorite, markFavorite, type PickerView } from "../ui/sessions/opencode-models";
import { effortChoices, modelChoices, settingsView, text, type ModelChoice, type ModelContext } from "../ui/sessions/session-model-settings";
import { tr } from "../i18n";

export interface ModelsPort {
  apply(target: Target, model: string, effort: string, pending: string | undefined, openCode: boolean): Promise<Outcome>;
  catalog(computer: string, folder: string, name: string): Promise<Record<string, unknown>>;
  favorite(computer: string, model: string, favorite: boolean, name: string): Promise<unknown>;
  notice(text: string): void;
}
interface Pending { key: string; target: Target; model: string; effort: string; id: string; openCode: boolean; attempted: boolean; scheduled: boolean; rearmed: boolean; blocked: boolean; applied: boolean }
export interface PendingRow { key: string; row?: Session; online: boolean; busy: boolean }
const targetOf = (i: ModelContext): Target => ({ computer: i.row.computer, name: i.row.name,
  run_id: text(i.details?.run_id) || i.row.runId || "", conversation_id: text(i.details?.conversation_id) || i.row.conversationId || "" });
const scope = (t: Target) => JSON.stringify([t.computer, t.name, t.run_id, t.conversation_id]);
const errorText = (e: unknown) => e instanceof Error ? e.message : String(e);

/** One popup, bounded pending identities, and the existing watch's Ready transitions. No timers or poller. */
export function createSessionModels(port: ModelsPort) {
  const [input, setInput] = createSignal<ModelContext>();
  const [opened, setOpened] = createSignal(false);
  const [model, setModel] = createSignal("");
  const [effort, setEffort] = createSignal("");
  const [extra, setExtra] = createSignal<ModelChoice[]>([]);
  const [changing, setChanging] = createSignal<string>();
  const [keys, setKeys] = createSignal<string[]>([]);
  const [pickerRev, setPickerRev] = createSignal(0);
  const [catalogError, setCatalogError] = createSignal("");
  let picker = new ModelPicker();
  let context = "", popupScope = "", generation = 0, reading = false;
  let catalog: Record<string, unknown> = {};
  const pending = new Map<string, Pending>();
  const confirmedFavorites = new Map<string, boolean>();
  // Receipts bridge the interval before the existing watch publishes the write.
  let overlay: { scope: string; receipt: Record<string, unknown> } | undefined;
  const publishKeys = () => setKeys([...pending.keys()]);
  const bump = () => setPickerRev((v) => v + 1);
  const view = () => { const i = input(); return i ? settingsView(i, changing() === i.row.key) : undefined; };
  const options = () => {
    const all = [...(view()?.options ?? [])];
    for (const m of extra()) if (!all.some((v) => v.id === m.id)) all.push(m);
    return all;
  };
  const efforts = () => { const v = view(); return v ? effortChoices(options(), model(), effort(), v.openCode, v.intendedModel, v.intendedEffort).choices : []; };
  const chooseModel = (id: string) => {
    const v = view(); if (!v) return;
    setModel(id);
    setEffort(effortChoices(options(), id, effort(), v.openCode, v.intendedModel, v.intendedEffort).value);
    picker.setCurrent(id); bump();
  };
  const canApply = () => {
    const v = view(), i = input();
    return !!v?.enabled && !!i && !changing() && opened() && popupScope === scope(targetOf(i))
      && options().some((m) => m.id === model()) && efforts().some((e) => e.value === effort())
      && (model() !== v.intendedModel || effort() !== v.intendedEffort || (v.pending && v.applyWhen === "now"));
  };

  async function readCatalog() {
    const i = input(), v = view();
    if (!i || !v?.openCode || !i.online || reading || !catalogSupported(i.details)) return;
    const mine = generation, target = scope(targetOf(i)), folder = text(i.details?.cwd) || i.row.cwdPath;
    reading = true;
    try {
      const result = await port.catalog(i.row.computer, folder, i.row.machine);
      if (mine !== generation || target !== context) return;
      catalog = result;
      for (const [id, starred] of confirmedFavorites) catalog = markCatalogFavorite(catalog, id, starred);
      picker.setCatalog(catalog); setCatalogError(""); bump();
    } catch (e) {
      if (mine !== generation || target !== context) return;
      setCatalogError(errorText(e)); picker.setCatalogFailed(errorText(e)); bump();
    } finally { if (mine === generation) reading = false; }
  }
  async function favorite(value: string, starred: boolean) {
    const i = input(); if (!i?.online || !view()?.enabled) return;
    const mine = generation;
    setCatalogError(""); picker.setFavoritePending(value, true); bump();
    try {
      await port.favorite(i.row.computer, value, starred, i.row.machine);
      if (mine !== generation) return;
      if (confirmedFavorites.size >= 64 && !confirmedFavorites.has(value)) confirmedFavorites.delete(confirmedFavorites.keys().next().value!);
      confirmedFavorites.set(value, starred);
      catalog = markCatalogFavorite(catalog, value, starred);
      picker.setFavorite(value, starred);
      const d = input()?.details;
      if (d) setInput({ ...input()!, details: { ...d, model_options: markFavorite(d.model_options, value, starred) } });
    } catch (e) { if (mine === generation) setCatalogError(errorText(e)); }
    finally { if (mine === generation) { picker.setFavoritePending(value, false); bump(); } }
  }
  const configurePicker = () => {
    picker.onChosen = (id) => {
      const native = picker.model(id);
      if (native) setExtra((all) => [{ id, label: native.label, effort_options: native.efforts, effort_labels: native.effortLabels }, ...all.filter((v) => v.id !== id)].slice(0, 64));
      chooseModel(id);
    };
    picker.onCatalogRequested = () => { void readCatalog(); };
    picker.onFavoriteRequested = (id, starred) => { void favorite(id, starred); };
  };
  configurePicker();

  function setContext(next: ModelContext | undefined) {
    const nextScope = next ? scope(targetOf(next)) : "";
    const nextContext = next ? JSON.stringify([nextScope, text(next.details?.cwd) || next.row.cwdPath]) : "";
    if (context !== nextScope || contextFolder !== nextContext) {
      context = nextScope; contextFolder = nextContext; generation++; reading = false;
      setOpened(false); setExtra([]); setCatalogError(""); catalog = {}; overlay = undefined;
      picker = new ModelPicker(); configurePicker(); confirmedFavorites.clear();
    }
    const previous = next ? pending.get(next.row.key) : undefined;
    if (next && previous?.applied && scope(previous.target) === nextScope && !text(next.details?.pending_settings_id)
      && text(next.details?.model) === previous.model && text(next.details?.effort) === previous.effort) {
      pending.delete(next.row.key); publishKeys();
    }
    if (next && overlay?.scope === nextScope) {
      const r = overlay.receipt, d = next.details;
      const id = text(r.pending_settings_id) || text(r.request_id);
      const confirmed = r.status === "scheduled" ? text(d?.pending_settings_id) === id
        : text(d?.model) === text(r.model) && text(d?.effort) === text(r.effort) && !text(d?.pending_settings_id);
      if (confirmed || (text(d?.pending_settings_id) && text(d?.pending_settings_id) !== id)) overlay = undefined;
      else next = { ...next, details: { ...d, ...(r.status === "scheduled"
        ? { pending_model: r.model, pending_effort: r.effort, pending_settings_id: id }
        : { model: r.model, settings_model: r.model, effort: r.effort, pending_model: "", pending_effort: "", pending_settings_id: "" }) } };
    }
    setInput(next);
    if (!next) return;
    const id = text(next.details?.pending_settings_id), model = text(next.details?.pending_model), effort = text(next.details?.pending_effort);
    const old = pending.get(next.row.key);
    if (id && (model || effort) && !(old?.blocked && scope(old.target) === nextScope) && (!old || old.id !== id || scope(old.target) !== nextScope)) {
      if (pending.size < 64 || old) {
        pending.set(next.row.key, { key: next.row.key, target: targetOf(next), model: model || view()!.model, effort, id,
          openCode: next.row.cmd === "opencode", attempted: false, scheduled: false, rearmed: false, blocked: false, applied: false }); publishKeys();
      }
    }
    if (view()?.openCode) {
      let short = next.details?.model_options;
      for (const [id, starred] of confirmedFavorites) short = markFavorite(short, id, starred);
      picker.setShortList(short, next.details?.model_options_truncated === true);
      picker.setFavoritesAvailable(next.online && catalogSupported(next.details));
      picker.setAllModelsAvailable(catalogSupported(next.details));
      if (!opened()) picker.setCurrent(view()!.intendedModel);
      bump();
    }
  }
  let contextFolder = "";

  async function send(change: Pending, automatic: boolean) {
    if (changing()) return;
    change.attempted = true; change.rearmed = false;
    setChanging(change.key);
    try {
      const out = await port.apply(change.target, change.model, change.effort, automatic ? change.id : undefined, change.openCode);
      if (out.outcome !== "done") {
        change.scheduled = false; change.blocked = true;
        const stale = automatic && ["pending settings changed; refresh before applying", "pending settings target changed; refresh before applying"].includes(out.detail.replace(/^hgs: /, ""));
        if (!stale) port.notice(tr("Model settings for %1: %2", change.target.name, out.detail));
        return;
      }
      const r = out.receipt as Record<string, unknown>;
      change.scheduled = r.status === "scheduled"; change.applied = r.status === "applied";
      if (change.scheduled) change.id = text(r.pending_settings_id) || text(r.request_id);
      // A first confirmed schedule grants one Ready attempt. A scheduled
      // automatic attempt needs a distinct NotReady -> Ready transition.
      if (change.scheduled && !automatic) change.attempted = false;
      change.model = text(r.model); change.effort = text(r.effort);
      if (context === scope(change.target)) {
        overlay = { scope: context, receipt: r };
        const i = input(); if (i) setContext(i);
      }
    } catch (e) {
      change.scheduled = false; change.blocked = true;
      port.notice(tr("Model settings for %1: %2", change.target.name, errorText(e)));
    } finally { setChanging(undefined); }
  }

  function observe(rows: PendingRow[]) {
    for (const { key, row, online, busy } of rows) {
      const p = pending.get(key); if (!p) continue;
      if (!row) { pending.delete(key); publishKeys(); continue; }
      if (p.blocked) continue;
      if (row.computer !== p.target.computer || row.name !== p.target.name || row.runId !== p.target.run_id
        || (row.conversationId && row.conversationId !== p.target.conversation_id)) continue;
      const ready = online && row.state === "running" && row.phase === "idle" && row.activity === "idle" && row.processState === "running";
      const i = input(), selected = i?.row.key === key;
      const inspectedReady = !selected || (scope(targetOf(i!)) === scope(p.target) && text(i?.details?.settings_apply_when) === "now" && view()?.enabled);
      if (!ready || (selected && text(i?.details?.settings_apply_when) !== "now")) {
        if (p.scheduled && p.attempted) p.rearmed = true;
        continue;
      }
      if (busy || changing() || !inspectedReady || (p.attempted && !(p.scheduled && p.rearmed))) continue;
      void send(p, true);
      break;
    }
  }

  return {
    setContext, observe, queuedKeys: keys, view, opened, model, effort, options, efforts, canApply, catalogError,
    changing: () => !!changing(),
    open() {
      const v = view(); if (!v) return;
      popupScope = context;
      setModel(v.intendedModel);
      setEffort(effortChoices(options(), v.intendedModel, v.intendedEffort, v.openCode, v.intendedModel, v.intendedEffort).value);
      picker.setCurrent(v.intendedModel); bump(); setOpened(true);
      if (v.openCode) void readCatalog();
    },
    close() { setOpened(false); }, chooseModel, chooseEffort: setEffort,
    picker: (): PickerView => { pickerRev(); return picker.view(); },
    pickerActions: { click: (id: string, star: boolean) => { if (view()?.enabled) { picker.click(id, star); bump(); } },
      select: (id: string) => { if (view()?.enabled) { picker.select(id); bump(); } }, toggleAll: () => { if (view()?.enabled) { picker.toggleAll(); bump(); } },
      search: (query: string) => { picker.setQuery(query); bump(); }, useNative() {}, close() { setOpened(false); } },
    async apply() {
      const i = input(); if (!i || !canApply()) return;
      const p: Pending = { key: i.row.key, target: targetOf(i), model: model(), effort: effort(), id: text(i.details?.pending_settings_id),
        openCode: i.row.cmd === "opencode", attempted: false, scheduled: false, rearmed: false, blocked: false, applied: false };
      if (pending.size >= 64 && !pending.has(p.key)) { port.notice(tr("Check pending model changes before applying another change.")); return; }
      pending.set(p.key, p); publishKeys(); setOpened(false);
      await send(p, false);
    },
  };
}

export const sessionModels = createSessionModels({
  apply: (target, model, effort, expected_pending_id, without_terminal) => bridge.runAction(target, { kind: "settings", model, effort, expected_pending_id, without_terminal }),
  catalog: (...args) => bridge.openCodeModels(...args), favorite: (...args) => bridge.openCodeFavorite(...args),
  notice: (message) => modelNotice?.(message),
});
let modelNotice: ((message: string) => void) | undefined;
export function bindModelNotice(notice: (message: string) => void) { modelNotice = notice; }
