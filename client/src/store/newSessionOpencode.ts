import type * as bridge from "../bridge";
import { tr } from "../i18n";
import { array, object, string, type Json } from "../ui/lib/json";
import {
  addModelOptions, catalogSupported, markCatalogFavorite, ModelPicker, updateText, type ModelOption, type PickerView,
} from "../ui/sessions/opencode-models";

// New session's OpenCode part (tray/src/NewSessionDialog.cpp 1145-1313 with
// 328-339 and 397-453): the model list and its picker, efforts, the folder
// catalogs, favorites and the explicit service start. It reads the dialog
// only through `OpenCodeHost`. Each folder read and star carries its own
// identity; only the newest read of a folder answers it, and a late answer
// stays with its own computer and folder. Nothing here starts OpenCode
// except its button.

/** What the OpenCode part reads and asks of the rest of the dialog. */
export interface OpenCodeHost {
  bridge: Pick<typeof bridge, "openCodeModels" | "openCodeFavorite" | "openCodeStart">;
  /** The selected computer: "" or the alias. */
  host(): string;
  /** The selected folder, "" for none. */
  path(): string;
  agent(): string;
  /** The computer as the dialog names it: the alias, else this computer's host name. */
  machineName(): string;
  /** The selected OpenCode row of the computer's catalog, `{}` for none. */
  openCodeProfile(): Json;
  online(computer: string): boolean;
  emit(): void;
  updateForm(): void;
  /** What Qt's slots touch beside the picker: the account list (430, 383, 1243, 1251) and the error line (452, 1310). */
  accountsLoading(): boolean;
  reloadAccounts(): void;
  /** A confirmed star marks the account row's `model_options` (438-443). */
  markRowFavorite(value: string, favorite: boolean): void;
  setAccountStatus(text: string): void;
  showError(text: string): void;
  /** Clears the error line while it still shows `text`. */
  clearError(text: string): void;
}

export interface EffortOption { value: string; label: string }

/** What the dialog shows of OpenCode. */
export interface OpenCodeView {
  models: ModelOption[];
  /** The selected model, "" for the native default. */
  model: string;
  modelVisible: boolean;
  modelButton: string;
  modelEnabled: boolean;
  pickerOpen: boolean;
  picker: PickerView;
  efforts: EffortOption[];
  effort: string;
  effortVisible: boolean;
  service: { visible: boolean; reason: string; offer: boolean; text: string; enabled: boolean; tooltip: string };
}

const EMPTY: Json = Object.freeze({}) as Json;
const nativeModel = (): ModelOption => ({ value: "", text: tr("Native default model"), label: "", efforts: [], effortLabels: [] });
const defaultEffort = (): EffortOption => ({ value: "", label: tr("Default") });
/** Folder catalogs one dialog keeps: a catalog can be large (1286-1294). */
const FOLDERS = 4;

const ready = (catalog: Json | undefined) => catalog?.catalog_state === "ready";
const valueOf = (entry: unknown) => { const o = object(entry); return typeof o.value === "string" ? o.value : string(o.id); };
const errorText = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));

export function createOpenCode(h: OpenCodeHost) {
  const b = h.bridge;
  let closed = false;
  let ticket = 0;
  const next = () => ++ticket;

  const picker = new ModelPicker();
  let models: ModelOption[] = [nativeModel()];
  let modelValue = "";
  let efforts: EffortOption[] = [defaultEffort()];
  let effortValue = "";
  let effortVisible = false;
  let pickerOpen = false;
  // The picker's context (computer and folder while OpenCode is chosen), the
  // catalog it shows, and the last choice made in it with its computer and folder.
  let modelContext = "";
  let chosenModel: Json | undefined;
  let chosenHost = "", chosenKey = "", pickerAvailability = "", favoriteError = "";
  let shownCatalog: Json = EMPTY;
  // `account models` answers per computer and folder, at most four.
  const folderCatalogs = new Map<string, Json>();
  let folderOrder: string[] = [];
  const folderRequests = new Map<number, string>();
  // The newest request issued per folder, while any request of it is pending.
  const folderLatest = new Map<string, number>();
  const favoriteRequests = new Map<number, { key: string; value: string; favorite: boolean; machine: string }>();
  let serviceStart = 0;
  let serviceHost = "";

  const modelKey = () => `${h.host()}\n${h.path()}`;
  const currentOption = () => models.find((m) => m.value === modelValue);
  const opencode = () => h.agent() === "opencode";

  /**
   * The short list is the account row's, or the folder's catalog once it is
   * ready. Every change of computer, folder or agent first resets the picker,
   * then sets its list, catalog and current model in the same turn (1148-1207).
   */
  function updateModels() {
    const enabled = opencode();
    const row = h.openCodeProfile();
    const key = modelKey();
    const contextKey = enabled ? key : "";
    const changed = contextKey !== modelContext;
    if (changed) {
      modelContext = contextKey;
      pickerOpen = false;
      picker.setCatalog({});
      shownCatalog = EMPTY;
      // A choice belongs to its computer; another folder keeps it only when its ready catalog lists it.
      const chosen = string(chosenModel?.value);
      const stored = folderCatalogs.get(key);
      const listed = () => [stored?.models, stored?.model_options].some((list) => array(list).some((entry) => valueOf(entry) === chosen));
      if (chosenHost !== h.host()) chosenModel = undefined;
      else if (enabled && chosen && chosenKey !== key) {
        if (ready(stored) && listed()) chosenKey = key;
        else chosenModel = undefined;
      }
    }
    // A new context starts from the choice it accepted above, else the native
    // default; within one context the selected model stays, listed or not.
    const selectedModel = changed ? string(chosenModel?.value) : modelValue;
    const current = currentOption();
    const kept = { value: selectedModel, label: current?.label ?? "", effort_options: current?.efforts ?? [], effort_labels: current?.effortLabels ?? [] };
    const catalog = folderCatalogs.get(key) ?? EMPTY;
    const isReady = enabled && ready(catalog);
    let options = [nativeModel()];
    if (enabled) {
      options = addModelOptions(options, row.model_options);
      if (isReady) options = addModelOptions(options, catalog.model_options);
      if (chosenModel) options = addModelOptions(options, [chosenModel]);
      if (!changed && selectedModel) options = addModelOptions(options, [kept]);
      const shortList = isReady ? catalog : row;
      picker.setShortList(shortList.model_options, shortList.model_options_truncated === true);
      // Another answer (a stopped service, a failure) shows its reason over the row's list.
      if (catalog !== shownCatalog) {
        shownCatalog = catalog;
        if ("failure" in catalog) picker.setCatalogFailed(string(catalog.failure));
        else if (Object.keys(catalog).length) picker.setCatalog(catalog);
      }
      const machine = h.machineName();
      const supported = catalogSupported(row), on = h.online(h.host());
      // Fleet polls reach here: stars are redrawn only when this changes.
      const availability = `${supported}${on}${machine}`;
      if (changed || availability !== pickerAvailability) {
        pickerAvailability = availability;
        picker.setAllModelsAvailable(supported, updateText(machine));
        picker.setFavoritesAvailable(supported && on, supported ? tr("%1 is offline.", machine) : updateText(machine));
      }
    }
    models = options;
    modelValue = options.some((m) => m.value === selectedModel) ? selectedModel : "";
    updateEfforts();
    updateModelButton();
  }

  /** A changed model keeps the chosen effort only when it offers that variant too (1209-1219). */
  function updateEfforts() {
    const selectedEffort = effortValue;
    const current = currentOption();
    const offered = current?.efforts ?? [];
    const enabled = opencode() && modelValue !== "" && offered.length > 0;
    efforts = [defaultEffort(), ...(enabled ? offered.map((value, i) => ({ value, label: current?.effortLabels[i] ?? value })) : [])];
    effortValue = efforts.some((e) => e.value === selectedEffort) ? selectedEffort : "";
    effortVisible = enabled;
  }

  const modelEnabled = () => opencode() && Object.keys(h.openCodeProfile()).length > 0;

  /** The picker opens only with this computer's OpenCode row: until then its context is not complete (1258-1265). */
  function updateModelButton() {
    if (!modelEnabled()) pickerOpen = false;
    picker.setCurrent(modelValue);
  }

  function chooseModelOption(value: string) {
    if (value === modelValue || !models.some((m) => m.value === value)) return;
    modelValue = value;
    updateEfforts();
    updateModelButton();
    h.updateForm();
  }

  function openModelPicker() {
    if (!modelEnabled()) return;
    pickerOpen = true;
    // Opening asks once per computer and folder; a stored answer is shown again.
    requestFolderCatalog(false);
    h.emit();
  }

  const pendingFor = (key: string) => [...folderRequests.values()].includes(key);

  /** `account models` for the folder, only from an hgs that has it on an online computer; nothing starts OpenCode (1277-1285). */
  function requestFolderCatalog(refresh: boolean) {
    const folder = h.path(), key = modelKey();
    if (!opencode() || !folder || !h.online(h.host()) || !catalogSupported(h.openCodeProfile())) return;
    if (!refresh && (folderCatalogs.has(key) || pendingFor(key))) return;
    const request = next();
    folderRequests.set(request, key);
    folderLatest.set(key, request);
    b.openCodeModels(h.host(), folder, h.machineName()).then(
      (answer) => folderAnswered(request, object(answer), undefined),
      (failure) => folderAnswered(request, undefined, errorText(failure)),
    );
  }

  /** Only the newest request issued for a folder answers it; a late answer stays with its own folder (397-422). */
  function folderAnswered(request: number, answer: Json | undefined, failure: string | undefined) {
    if (closed || !folderRequests.has(request)) return;
    const key = folderRequests.get(request) as string;
    folderRequests.delete(request);
    const latest = folderLatest.get(key) === request;
    // With nothing pending for this folder, no older answer can arrive any more.
    if (!pendingFor(key)) folderLatest.delete(key);
    if (!latest) return;
    if (answer) {
      storeFolderCatalog(key, answer);
      if (key === modelKey() && opencode()) updateModels();
    } else {
      // A failed refresh keeps a ready catalog; otherwise the failure is shown again, not asked again.
      const kept = ready(folderCatalogs.get(key));
      if (!kept) storeFolderCatalog(key, { failure });
      if (key === modelKey() && opencode()) {
        if (kept) picker.setCatalogFailed(failure ?? "");
        else updateModels();
      }
    }
    h.emit();
  }

  /** Answers of at most four folders. The current one stays. */
  function storeFolderCatalog(key: string, answer: Json) {
    folderOrder = [...folderOrder.filter((k) => k !== key), key];
    folderCatalogs.set(key, answer);
    while (folderOrder.length > FOLDERS) {
      const oldest = folderOrder[0] === modelKey() ? folderOrder[1] : folderOrder[0];
      drop(oldest);
    }
  }

  function drop(key: string) {
    folderOrder = folderOrder.filter((k) => k !== key);
    folderCatalogs.delete(key);
  }

  /** The picker's choice is the only way its model becomes the selection (1296-1304). */
  function chooseModel(value: string) {
    const model = picker.model(value);
    if (!model || !opencode()) return;
    chosenModel = { value: model.value, label: model.label, effort_options: model.efforts, effort_labels: model.effortLabels };
    chosenHost = h.host();
    chosenKey = modelKey();
    models = addModelOptions(models, [chosenModel]);
    pickerOpen = false;
    const index = models.findIndex((m) => m.value === value);
    const target = models[Math.max(0, index)].value;
    if (target !== modelValue) { modelValue = target; updateEfforts(); updateModelButton(); }
    h.updateForm();
  }

  /** The picker's All models: it reads the folder again unless a ready catalog is here (328-337). */
  function catalogRequested() {
    const machine = h.machineName();
    const stored = folderCatalogs.get(modelKey());
    if (!h.path()) picker.setCatalogFailed(tr("Choose a working folder to list all of its models."));
    else if (!h.online(h.host())) picker.setCatalogFailed(`${machine} is offline.`);
    else if (ready(stored)) picker.setCatalog(stored as Json);
    else if (!pendingFor(modelKey())) requestFolderCatalog(true);
  }

  /** One request identity per star click; a failure is shown, never retried (1306-1313). */
  function changeFavorite(value: string, favorite: boolean) {
    if (!opencode() || !h.online(h.host()) || !catalogSupported(h.openCodeProfile())) return;
    if (favoriteError) h.clearError(favoriteError);
    favoriteError = "";
    picker.setFavoritePending(value, true);
    const request = next();
    const machine = h.host();
    favoriteRequests.set(request, { key: modelKey(), value, favorite, machine });
    b.openCodeFavorite(machine, value, favorite, h.machineName()).then(() => favoriteDone(request), (failure) => favoriteFailed(request, errorText(failure)));
  }

  function favoriteDone(request: number) {
    const change = favoriteRequests.get(request);
    if (closed || !change) return;
    favoriteRequests.delete(request);
    // Favorites belong to the computer: its other folders' catalogs are stale now.
    for (const key of [...folderOrder]) if (key.startsWith(`${change.machine}\n`) && key !== modelKey()) drop(key);
    if (change.machine !== h.host()) { h.emit(); return; }
    h.reloadAccounts();
    // The answer for another folder or agent leaves this picker as it is.
    if (change.key !== modelKey() || !opencode()) { h.emit(); return; }
    // The stored catalog and the account row carry the confirmed change, or the
    // picker's next short list would bring back the state before it.
    const stored = folderCatalogs.get(change.key);
    if (ready(stored)) folderCatalogs.set(change.key, markCatalogFavorite(stored as Json, change.value, change.favorite));
    h.markRowFavorite(change.value, change.favorite);
    picker.setFavorite(change.value, change.favorite);
    requestFolderCatalog(true);
    h.emit();
  }

  function favoriteFailed(request: number, failure: string) {
    const change = favoriteRequests.get(request);
    if (closed || !change) return;
    favoriteRequests.delete(request);
    if (change.machine !== h.host() || change.key !== modelKey() || !opencode()) return;
    picker.setFavoritePending(change.value, false);
    favoriteError = tr("Could not change the favorite on %1: %2", change.machine || h.machineName(), String(failure));
    h.showError(favoriteError);
    h.emit();
  }

  picker.onChosen = chooseModel;
  picker.onFavoriteRequested = changeFavorite;
  picker.onCatalogRequested = catalogRequested;

  const serviceEnabled = () => h.online(h.host()) && !(serviceStart && serviceHost === h.host()) && !h.accountsLoading();

  /** The service row (1229-1246). */
  function service(): OpenCodeView["service"] {
    const profile = h.openCodeProfile();
    const state = string(profile.catalog_state);
    const starting = serviceStart !== 0 && serviceHost === h.host();
    const present = Object.keys(profile).length > 0;
    const installed = profile.installed !== false;
    let reason = "";
    if (present && !installed) reason = string(profile.install_reason);
    else if (present && state && state !== "ready") reason = string(profile.catalog_reason);
    const offer = starting || (state === "not_running" && installed);
    const machine = h.machineName();
    return {
      visible: offer || reason !== "", reason, offer,
      text: starting ? tr("Starting OpenCode…") : tr("Start OpenCode on %1", machine),
      enabled: serviceEnabled(),
      tooltip: h.online(h.host()) ? tr("Start the shared OpenCode service on this computer. Zerus never stops it.") : tr("%1 is offline.", machine),
    };
  }

  /** Explicit only: hgs starts the shared service when none is registered and alive (1248-1254). */
  function startOpenCode() {
    if (serviceStart || !serviceEnabled() || string(h.openCodeProfile().catalog_state) !== "not_running") return;
    serviceHost = h.host();
    h.setAccountStatus("");
    const request = next();
    serviceStart = request;
    const computer = serviceHost;
    b.openCodeStart(computer).then(() => {
      if (closed || serviceStart !== request) return;
      serviceStart = 0;
      // The service answered with its catalog: read this computer's accounts again.
      if (computer === h.host()) h.reloadAccounts();
      h.emit();
    }, (failure) => {
      if (closed || serviceStart !== request) return;
      serviceStart = 0;
      if (computer === h.host()) h.setAccountStatus(tr("Could not start OpenCode: %1", errorText(failure)));
      h.emit();
    });
    h.emit();
  }

  return {
    updateModels,
    /** The model to start with: only for OpenCode, "" for the native default. */
    model: () => (opencode() ? modelValue : ""),
    effort: () => effortValue,
    /** Refresh asks again for this computer's folders whose last answer was not a ready catalog (317-322). */
    dropUnready() {
      for (const key of [...folderOrder]) if (key.startsWith(`${h.host()}\n`) && !ready(folderCatalogs.get(key))) drop(key);
    },
    /** Edit choices…: the recipe's model with its effort (801-806). */
    restore(model: string, effort: string) {
      chosenModel = { value: model, label: model, effort_options: effort ? [effort] : [], effort_labels: effort ? [effort] : [] };
      chosenHost = h.host();
      chosenKey = modelKey();
      updateModels();
    },
    view(): OpenCodeView {
      return {
        models, model: modelValue, modelVisible: opencode(),
        modelButton: modelValue === "" ? tr("Native default model") : currentOption()?.label || modelValue,
        modelEnabled: modelEnabled(), pickerOpen, picker: picker.view(),
        efforts, effort: effortValue, effortVisible, service: service(),
      };
    },
    actions: {
      chooseModel: chooseModelOption,
      chooseEffort(value: string) { if (efforts.some((e) => e.value === value)) { effortValue = value; h.updateForm(); } },
      openModelPicker,
      closeModelPicker() { pickerOpen = false; h.emit(); },
      useNativeModel() {
        chosenModel = undefined;
        pickerOpen = false;
        if (modelValue !== "") { modelValue = ""; updateEfforts(); updateModelButton(); }
        h.updateForm();
      },
      pickerClick(value: string, star: boolean) { picker.click(value, star); h.emit(); },
      pickerSelect(value: string) { picker.select(value); h.emit(); },
      pickerToggleAll() { picker.toggleAll(); h.emit(); },
      pickerSearch(text: string) { picker.setQuery(text); h.emit(); },
      startOpenCode,
    },
    dispose() { closed = true; },
  };
}
