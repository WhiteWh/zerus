import { For, Index, Show, createEffect, createMemo, createSignal, on, onMount } from "solid-js";
import type { JSX } from "solid-js";
import { tr, trNoop } from "../../i18n";
import { Icon } from "../lib/icons";
import { LISTS, createCatalogEditor, type CatalogEditorStore } from "../../store/catalogEditor";
import { teamRoleModels } from "../../bridge";
import type { CatalogFieldSpec, CatalogKind, CatalogList } from "../../types";
import type { CatalogProposal } from "./catalog";
import {
  ICON_GLYPH, ROLE_ICONS, SWATCHES, agentChange, chosenIcon, chosenSwatch, createModelCatalogs, dropBlock, effortOptions, fieldDiff, iconValue,
  includeBlock, modelOptions, moveBlock, plateAnswers, promptBlocks, proposalMark, proposalOf, swatchColor, type ModelChoice, type PromptChange,
} from "./catalogControls";
import { normalizeModelCatalog, selectModel } from "../provider-selection";
import "../styles/catalog.css";
import { TextInput, TextArea } from "../lib/TextField";
import { Dropdown } from "../lib/Dropdown";
import { accountsPage, loadAccounts } from "../../store/accounts";
import { accountChoices } from "../../store/newTeamSources";

// Task #44: Qt's TeamCatalogEditor. The header holds Open Team designer,
// Import…, Export…, Revert, Save catalog, Discard draft and Apply
// (TeamCatalogEditor.cpp:517-523); the tabs edit roles, templates, routing
// rules, the lead's planning rules and pipelines. Every field comes from
// core's schema and every edit is checked there: nothing here reads or builds
// a catalog. Confirmations go through TeamConfirmDialog, Cancel focused.

type Tab = "roles" | "templates" | "routing" | "rules" | "pipelines";
type Item = Record<string, unknown>;

const TABS: { id: Tab; label: string }[] = [
  { id: "roles", label: trNoop("Roles") },
  { id: "templates", label: trNoop("Templates") },
  { id: "routing", label: trNoop("Routing") },
  { id: "rules", label: trNoop("Rules") },
  { id: "pipelines", label: trNoop("Pipelines") },
];
const KIND: Partial<Record<Tab, CatalogKind>> = { roles: "role", templates: "template", routing: "rule", pipelines: "pipeline" };
const ADD: Partial<Record<Tab, string>> = { roles: trNoop("+ Role"), templates: trNoop("+ Template"), routing: trNoop("+ Rule"), pipelines: trNoop("+ Pipeline") };

export interface CatalogDraftControls {
  kept: number;
  undecided: number;
  apply: () => void;
  discard: () => void;
}

export interface TeamCatalogEditorProps {
  /** The editor's store; one of its own when absent (this computer). */
  editor?: CatalogEditorStore;
  /** The followed catalog's version: a newer one is read again, unsaved edits rebased. */
  savedVersion?: number;
  /** The designer's draft, when there is one: Discard draft and Apply. */
  draft?: CatalogDraftControls;
  onOpenDesigner?: () => void;
  /** Task 50: the designer's proposals, drawn as a plate on each item they touch, and their Keep/Reject/Undo. */
  proposals?: CatalogProposal[];
  decide?: (proposal: string, decision: "keep" | "reject" | "undo") => void;
  /** Task 50: an account's models for the model and effort menus (default: the core's team_role_models). */
  models?: (computer: string, agent: string, account: string) => Promise<ModelChoice[]>;
  /** The appearance the swatches are drawn for. */
  dark?: boolean;
}

/** What a role's own controls need from the editor. */
interface RoleContext {
  catalogs: ReturnType<typeof createModelCatalogs>;
  tick: () => number;
  dark: boolean;
}

const str = (value: unknown) => (typeof value === "string" ? value : value == null ? "" : String(value));
const words = (value: unknown) => (Array.isArray(value) ? value.map(str).join(", ") : str(value));
const splitWords = (text: string) => text.split(",").map((word) => word.trim()).filter(Boolean);

export function TeamCatalogEditor(props: TeamCatalogEditorProps): JSX.Element {
  const editor = props.editor ?? createCatalogEditor();
  const [modelTick, setModelTick] = createSignal(0);
  const roles: RoleContext = {
    catalogs: createModelCatalogs((computer, agent, account) => (props.models ?? teamRoleModels)(computer, agent, account), () => setModelTick((n) => n + 1)),
    tick: modelTick,
    dark: props.dark ?? true,
  };
  const [tab, setTab] = createSignal<Tab>("roles");
  const [current, setCurrent] = createSignal<Partial<Record<Tab, string>>>({});
  onMount(() => { void editor.load(); void loadAccounts(); });
  createEffect(on(() => props.savedVersion, (version, previous) => { if (version !== undefined && previous !== undefined && version !== previous) void editor.load(); }));

  const list = (which: CatalogList): Item[] => (editor.view()?.[which] as Item[] | undefined) ?? [];
  const items = createMemo(() => { const kind = KIND[tab()]; return kind ? list(LISTS[kind]) : []; });
  const chosen = createMemo(() => {
    const all = items(), id = current()[tab()];
    return all.find((item) => item.id === id) ?? all[0];
  });
  const choose = (id: string) => setCurrent({ ...current(), [tab()]: id });
  const unsaved = () => editor.view()?.unsaved ?? 0;
  const edited = (which: CatalogList, id: unknown) => !!editor.view()?.edited[which]?.includes(str(id));

  const add = async () => {
    const kind = KIND[tab()];
    if (!kind) return;
    const before = new Set(items().map((item) => str(item.id)));
    if (await editor.edit({ op: "add", kind })) {
      const fresh = items().find((item) => !before.has(str(item.id)));
      if (fresh) choose(str(fresh.id));
    }
  };
  const set = (kind: CatalogKind, id: string, field: string, value: unknown) => editor.edit({ op: "set", kind, id, field, value });

  return (
    <section class="cat cat-editor" aria-label={tr("Team catalog")}>
      <header class="cat-head">
        <h2>{tr("Team catalog")}</h2>
        <span class="cat-version">{editor.view()?.builtin ? tr("Built-in catalog") : tr("v%1", editor.view()?.version ?? 0)}</span>
        <Show when={unsaved() > 0}>
          <span class="cat-unsaved">{unsaved() === 1 ? tr("%1 unsaved change", unsaved()) : tr("%1 unsaved changes", unsaved())}</span>
        </Show>
        <Show when={editor.notice()}>{(notice) => <span class="cat-notice" classList={{ error: notice().error }} role={notice().error ? "alert" : "status"}>{notice().text}</span>}</Show>
        <span class="grow" />
        <Show when={props.onOpenDesigner}>
          <button type="button" class="btn ghost small" onClick={() => props.onOpenDesigner?.()}><Icon name="users" size={13} />{tr("Open Team designer")}</button>
        </Show>
        <button type="button" class="btn ghost small" disabled={editor.busy()} onClick={() => void editor.importFile()}><Icon name="folder-open" size={13} />{tr("Import…")}</button>
        <button type="button" class="btn ghost small" disabled={editor.busy() || !editor.view()} onClick={() => void editor.exportFile()}><Icon name="download" size={13} />{tr("Export…")}</button>
        <button type="button" class="btn ghost small" disabled={editor.busy() || unsaved() === 0} onClick={() => void editor.revert()}>{tr("Revert")}</button>
        <button type="button" class="btn small primary" disabled={editor.busy() || unsaved() === 0} onClick={() => void editor.save()}>{tr("Save catalog")}</button>
        <Show when={props.draft}>
          {(draft) => (
            <>
              <button type="button" class="btn ghost small" disabled={editor.busy()} onClick={() => draft().discard()}>{tr("Discard draft")}</button>
              <button type="button" class="btn small" disabled={editor.busy() || draft().kept === 0 || unsaved() > 0} onClick={() => draft().apply()}
                title={unsaved() > 0 ? tr("Save or revert your edits first") : tr("Write the kept changes as the next catalog version")}>{tr("Apply")}</button>
            </>
          )}
        </Show>
      </header>

      <nav class="cat-tabs" role="tablist" aria-label={tr("Catalog sections")}>
        <For each={TABS}>{(entry) => (
          <button type="button" role="tab" class="cat-tab" aria-selected={tab() === entry.id} classList={{ on: tab() === entry.id }} onClick={() => setTab(entry.id)}>{tr(entry.label)}</button>
        )}</For>
      </nav>

      <Show when={editor.view() && editor.schema()} fallback={<p class="cat-empty">{editor.busy() ? tr("Reading the catalog…") : tr("The catalog is not read yet.")}</p>}>
        <Show when={tab() === "rules"} fallback={
          <Show when={tab() === "routing"} fallback={
            <div class="cat-body">
              <aside class="cat-list" aria-label={tr(TABS.find((entry) => entry.id === tab())!.label)}>
                <For each={items()}>{(item) => (
                  <button type="button" class="cat-row" classList={{ on: chosen()?.id === item.id, edited: edited(LISTS[KIND[tab()]!], item.id) }} onClick={() => choose(str(item.id))}>
                    <span class="cat-row-name">{str(item.name) || str(item.id)}</span>
                    <span class="cat-row-meta">{str(item.about ?? item.description ?? "")}</span>
                  </button>
                )}</For>
                <button type="button" class="btn ghost small cat-add" disabled={editor.busy()} onClick={() => void add()}>{tr(ADD[tab()]!)}</button>
              </aside>
              <div class="cat-form">
                <Show when={chosen()} fallback={<p class="cat-empty">{tr("Nothing here yet.")}</p>}>
                  {(item) => <ItemForm editor={editor} kind={KIND[tab()]!} item={item()} set={set} roles={roles}
                    proposal={proposalOf(props.proposals, KIND[tab()]!, str(item().id))} decide={props.decide} />}
                </Show>
              </div>
            </div>
          }>
            <Routing editor={editor} set={set} add={add} />
          </Show>
        }>
          <PlanningRules editor={editor} />
        </Show>
      </Show>
    </section>
  );
}

/** Task 50 (Qt proposalPlate): the designer's proposal on this item: its mark, title, Keep/Reject or Undo, reason and what it changes. */
export function ProposalPlate(props: { proposal: CatalogProposal; item?: Item; decide?: (proposal: string, decision: "keep" | "reject" | "undo") => void }) {
  const mark = () => proposalMark(props.proposal);
  const diff = () => fieldDiff(props.item, props.proposal);
  const label = (decision: "keep" | "reject" | "undo") => (decision === "keep" ? tr("Keep") : decision === "reject" ? tr("Reject") : tr("Undo"));
  return (
    <div class="cat-plate" data-proposal={props.proposal.id} data-state={props.proposal.state}>
      <div class="cat-plate-head">
        <span class={`cat-mark tone-${mark().tone}`}>{mark().text}</span>
        <b class="cat-plate-title" classList={{ struck: props.proposal.state === "rejected" }}>{props.proposal.title}</b>
        <span class="grow" />
        <Show when={props.proposal.state !== "proposed"}>
          <span class={props.proposal.state === "kept" ? "cat-kept" : "cat-rejected"}>{props.proposal.state === "kept" ? tr("Kept") : tr("Rejected")}</span>
        </Show>
        <For each={plateAnswers(props.proposal.state)}>{(decision) => (
          <button type="button" class="btn small" classList={{ ghost: decision !== "keep" }} disabled={!props.decide}
            onClick={() => props.decide?.(props.proposal.id, decision)}>{label(decision)}</button>
        )}</For>
      </div>
      <Show when={props.proposal.reason}><p class="cat-hint">{props.proposal.reason}</p></Show>
      <Show when={diff().length > 0}>
        <dl class="cat-diff">
          <For each={diff()}>{(change) => (
            <div class="cat-diff-row" data-field={change.field}>
              <dt>{change.field}</dt>
              <dd><Show when={change.from}><del>{change.from}</del></Show><ins>{change.to || tr("(empty)")}</ins></dd>
            </div>
          )}</For>
        </dl>
      </Show>
    </div>
  );
}

/** Task 50 (Qt teamRoleSwatch/teamRoleIcon): the palette's swatches and the role icons; a click stores the name. */
export function LookPicker(props: { item: Item; dark: boolean; onColor: (name: string) => void; onIcon: (value: string) => void }) {
  const color = () => chosenSwatch(props.item);
  const icon = () => chosenIcon(props.item);
  return (
    <div class="cat-look">
      <div class="cat-swatches" role="radiogroup" aria-label={tr("Color")}>
        <For each={SWATCHES}>{(swatch) => (
          <button type="button" role="radio" class="cat-swatch" aria-checked={swatch.name === color()} aria-label={swatch.name} title={swatch.name}
            data-swatch={swatch.name} style={{ background: props.dark ? swatch.dark : swatch.light }} classList={{ on: swatch.name === color() }}
            onClick={() => props.onColor(swatch.name)} />
        )}</For>
      </div>
      <div class="cat-icons" role="radiogroup" aria-label={tr("Icon")}>
        <For each={ROLE_ICONS}>{(name) => (
          <button type="button" role="radio" class="cat-icon" aria-checked={name === icon()} aria-label={name} title={name} data-icon={name}
            classList={{ on: name === icon() }} style={name === icon() ? { color: swatchColor(color(), props.dark) || undefined } : undefined}
            onClick={() => props.onIcon(iconValue(name))}><Icon name={ICON_GLYPH[name]} size={14} /></button>
        )}</For>
      </div>
    </div>
  );
}

/** Task 50 (Qt promptOrder): the blocks in order, each with include and moves; a row may be dragged to its place. */
export function PromptOrder(props: { item: Item; apply: (change: PromptChange) => void }) {
  const [dragged, setDragged] = createSignal<number | null>(null);
  const blocks = () => promptBlocks(props.item);
  const apply = (change: PromptChange | undefined) => { if (change) props.apply(change); };
  return (
    <div class="cat-prompt-order" role="list" aria-label={tr("Prompt order")}>
      <For each={blocks()}>{(block, index) => (
        <div class="cat-prompt-block" role="listitem" data-block={block.key} classList={{ off: block.off, dragging: dragged() === index() }} draggable="true"
          onDragStart={(event) => { setDragged(index()); event.dataTransfer?.setData("text/plain", block.key); }}
          onDragOver={(event) => event.preventDefault()}
          onDrop={(event) => { event.preventDefault(); const from = dragged(); setDragged(null); if (from !== null) apply(dropBlock(props.item, from, index())); }}
          onDragEnd={() => setDragged(null)}>
          <Icon name="grip-vertical" size={13} />
          <span class="cat-prompt-title">{tr("%1. %2", index() + 1, block.title)}</span>
          <span class="grow" />
          <label class="cat-prompt-include"><input type="checkbox" checked={!block.off} onChange={(event) => apply(includeBlock(props.item, block.key, event.currentTarget.checked))} />{tr("include")}</label>
          <button type="button" class="icon-btn small" aria-label={tr("Move up")} disabled={index() === 0} onClick={() => apply(moveBlock(props.item, index(), -1))}><Icon name="arrow-up" size={12} /></button>
          <button type="button" class="icon-btn small" aria-label={tr("Move down")} disabled={index() === blocks().length - 1} onClick={() => apply(moveBlock(props.item, index(), 1))}><Icon name="arrow-down" size={12} /></button>
        </div>
      )}</For>
      <span class="cat-hint">{tr("Blocks are sent top to bottom; switched-off blocks are skipped.")}</span>
    </div>
  );
}

export function AccountMenu(props: { item: Item; value: string; onChange: (value: string) => void; label: string }) {
  const options = () => {
    const choices = accountChoices(accountsPage(), str(props.item.computer))
      .filter((choice) => choice.provider === str(props.item.agent))
      .map((choice) => ({ value: choice.id, label: choice.label, disabled: !choice.signedIn }));
    const all = [{ value: "", label: tr("Default account") }, ...choices];
    if (props.value && !all.some((choice) => choice.value === props.value)) all.push({ value: props.value, label: props.value, disabled: false });
    return all;
  };
  return <Dropdown class="cat-account" value={props.value} aria-label={props.label} onChange={props.onChange} options={options()} />;
}

const CUSTOM_MODEL = "__custom_model__";
/** Task 50 (Qt teamRoleModel/teamRoleEffort): the account's models and the selected model's efforts. */
export function ModelMenu(props: { item: Item; roles: RoleContext; value: string; effort: string; onChange: (value: string) => void; onEffortChange: (value: string) => void; label: string }) {
  const [custom, setCustom] = createSignal(false);
  const options = () => {
    props.roles.tick();
    return [{ value: "", label: tr("default model") },
      ...modelOptions(props.item, props.roles.catalogs.get(props.item)).map((option) => ({ value: option.id, label: option.label })),
      { value: CUSTOM_MODEL, label: tr("Custom") }];
  };
  onMount(() => props.roles.catalogs.request(props.item));
  createEffect(on(() => `${str(props.item.computer)}\n${str(props.item.account)}\n${str(props.item.agent)}`, () => { setCustom(false); props.roles.catalogs.request(props.item); }, { defer: true }));
  const choose = (value: string) => {
    const next = selectModel(normalizeModelCatalog(props.roles.catalogs.get(props.item) ?? []), value, props.effort);
    props.onChange(next.modelId);
    props.onEffortChange(next.effortId);
  };
  const select = (value: string) => {
    if (value === CUSTOM_MODEL) { setCustom(true); return; }
    setCustom(false);
    choose(value);
  };
  return (
    <>
      <Dropdown class="cat-model" value={custom() ? CUSTOM_MODEL : props.value} aria-label={props.label} onChange={select} options={options()} />
      <Show when={custom()}><TextInput class="cat-model-custom" value={props.value} placeholder={tr("Custom")} aria-label={props.label}
        onChange={(event) => choose(event.currentTarget.value.trim())} /></Show>
    </>
  );
}

export function EffortMenu(props: { item: Item; roles: RoleContext; model: string; value: string; onChange: (value: string) => void; label: string }) {
  const options = () => (props.roles.tick(), effortOptions(props.model, props.roles.catalogs.get(props.item), props.value));
  return <Dropdown class="cat-effort" value={props.value} aria-label={props.label} onChange={props.onChange}
    options={options().map((effort) => ({ value: effort, label: effort || "—" }))} />;
}

/** One item's fields, Duplicate and Delete. */
function ItemForm(props: { editor: CatalogEditorStore; kind: CatalogKind; item: Item; set: (kind: CatalogKind, id: string, field: string, value: unknown) => Promise<boolean>;
  roles: RoleContext; proposal?: CatalogProposal; decide?: (proposal: string, decision: "keep" | "reject" | "undo") => void }) {
  const specs = () => (props.editor.schema()?.[LISTS[props.kind]] ?? [])
    // Task 50: the switched-off blocks are part of Prompt order; Icon is drawn with Color.
    .filter((spec) => props.kind !== "role" || (spec.name !== "prompt_off" && spec.name !== "icon"));
  const id = () => str(props.item.id);
  const setField = (field: string, value: unknown) => props.set(props.kind, id(), field, value);
  const applyPrompt = async (change: PromptChange) => {
    if (change.task_template !== undefined && !(await setField("task_template", change.task_template))) return;
    if (await setField("prompt_order", change.prompt_order)) await setField("prompt_off", change.prompt_off);
  };
  /** Another agent: its first model and the default account (Qt teamRoleAgent). */
  const changeAgent = async (agent: unknown): Promise<boolean> => {
    const next = agentChange(str(agent));
    if (!(await setField("agent", next.agent))) return false;
    await setField("account", next.account);
    await setField("model", next.model);
    return setField("effort", "");
  };
  const changeAccount = async (account: unknown): Promise<boolean> => {
    if (!(await setField("account", account))) return false;
    await setField("model", "");
    return setField("effort", "");
  };
  const special = (spec: CatalogFieldSpec): JSX.Element | undefined => {
    if (props.kind !== "role") return undefined;
    switch (spec.name) {
      case "color":
        return <LookPicker item={props.item} dark={props.roles.dark} onColor={(name) => void setField("color", name)} onIcon={(value) => void setField("icon", value)} />;
      case "account":
        return <AccountMenu item={props.item} value={str(props.item.account)} label={tr(spec.label)} onChange={(value) => void changeAccount(value)} />;
      case "model":
        return <ModelMenu item={props.item} roles={props.roles} value={str(props.item.model)} effort={str(props.item.effort)} label={tr(spec.label)} onChange={(value) => void setField("model", value)} onEffortChange={(value) => void setField("effort", value)} />;
      case "effort":
        return <EffortMenu item={props.item} roles={props.roles} model={str(props.item.model)} value={str(props.item.effort)} label={tr(spec.label)} onChange={(value) => void setField("effort", value)} />;
      case "prompt_order":
        return <PromptOrder item={props.item} apply={(change) => void applyPrompt(change)} />;
      default:
        return undefined;
    }
  };
  return (
    <div class="cat-item">
      <div class="cat-item-head">
        <b>{str(props.item.name) || id()}</b>
        <span class="cat-id">{id()}</span>
        <span class="grow" />
        <button type="button" class="btn ghost small" disabled={props.editor.busy()} onClick={() => void props.editor.edit({ op: "duplicate", kind: props.kind, id: id() })}><Icon name="copy" size={12} />{tr("Duplicate")}</button>
        <button type="button" class="btn ghost small danger" disabled={props.editor.busy()} onClick={() => void props.editor.remove(props.kind, id())}><Icon name="x" size={12} />{tr("Delete")}</button>
      </div>
      <Show when={props.proposal}>{(proposal) => <ProposalPlate proposal={proposal()} item={props.item} decide={props.decide} />}</Show>
      <For each={specs()}>{(spec) => (
        <Field editor={props.editor} spec={spec} value={props.item[spec.name]} custom={special(spec)} roleItem={props.kind === "role" ? props.item : undefined} roles={props.roles}
          onChange={(value) => (props.kind === "role" && spec.name === "agent" ? changeAgent(value) : props.set(props.kind, id(), spec.name, value))}
          onEffortChange={(value) => props.set(props.kind, id(), "effort", value)} />
      )}</For>
    </div>
  );
}

/** The ids a field names, for a choice instead of free text. */
function references(editor: CatalogEditorStore, spec: CatalogFieldSpec): [string, string][] | undefined {
  const view = editor.view();
  if (!view) return undefined;
  const named = (entries: Record<string, unknown>[]) => entries.map((entry) => [str(entry.id), str(entry.name) || str(entry.id)] as [string, string]);
  if (spec.name === "role") return [["lead", tr("Lead")], ...named(view.roles)];
  if (spec.name === "template") return [["", tr("No template")], ...named(view.templates)];
  if (spec.name === "pipeline") return [["", tr("No pipeline")], ...named(view.pipelines)];
  return undefined;
}

/** One field, drawn from its schema type; each change goes to core at once. */
function Field(props: { editor: CatalogEditorStore; spec: CatalogFieldSpec; value: unknown; onChange: (value: unknown) => Promise<boolean> | void; onEffortChange?: (value: unknown) => Promise<boolean> | void; compact?: boolean;
  /** Task 50: a control drawn instead of the schema's plain one. */
  custom?: JSX.Element;
  /** Task 50: the role whose rows (Models) get the model and effort menus. */
  roleItem?: Item; roles?: RoleContext; row?: Item }) {
  const label = () => (props.spec.name === "color" && props.custom ? tr("Icon / color") : tr(props.spec.label));
  const fieldId = `cat-field-${Math.random().toString(36).slice(2)}`;
  const input = (): JSX.Element => {
    const spec = props.spec;
    if (props.custom) return props.custom;
    // A Models row: the role's account models and that model's efforts.
    if (props.roleItem && props.roles && props.row && spec.name === "model")
      return <ModelMenu item={props.roleItem} roles={props.roles} value={str(props.value)} effort={str(props.row.effort)} label={label()} onChange={(value) => void props.onChange(value)} onEffortChange={(value) => void props.onEffortChange?.(value)} />;
    if (props.roleItem && props.roles && props.row && spec.name === "effort")
      return <EffortMenu item={props.roleItem} roles={props.roles} model={str(props.row.model)} value={str(props.value)} label={label()} onChange={(value) => void props.onChange(value)} />;
    const refs = spec.type === "text" ? references(props.editor, spec) : undefined;
    if (refs) {
      const value = str(props.value);
      const options = refs.some(([id]) => id === value) ? refs : [...refs, [value, value] as [string, string]];
      return <select id={fieldId} value={value} onChange={(event) => void props.onChange(event.currentTarget.value)}>
        <For each={options}>{([id, name]) => <option value={id}>{name}</option>}</For>
      </select>;
    }
    switch (spec.type) {
      case "multiline":
        return <TextArea id={fieldId} rows={5} value={str(props.value)} onChange={(event) => void props.onChange(event.currentTarget.value)} />;
      case "number":
        return <TextInput id={fieldId} type="number" min={spec.min} max={spec.max} value={str(props.value ?? spec.min ?? 0)}
          onChange={(event) => void props.onChange(Number.parseInt(event.currentTarget.value, 10) || 0)} />;
      case "bool":
        return <input id={fieldId} type="checkbox" checked={props.value === true} onChange={(event) => void props.onChange(event.currentTarget.checked)} />;
      case "choice":
        return <select id={fieldId} value={str(props.value)} onChange={(event) => void props.onChange(event.currentTarget.value)}>
          <For each={spec.options ?? []}>{([value, name]) => <option value={value}>{tr(name)}</option>}</For>
        </select>;
      case "words":
        return <TextInput id={fieldId} value={words(props.value)} placeholder={tr("Comma-separated")} onChange={(event) => void props.onChange(splitWords(event.currentTarget.value))} />;
      case "list":
        return <Rows editor={props.editor} spec={spec} value={Array.isArray(props.value) ? (props.value as Item[]) : []} onChange={props.onChange} roleItem={props.roleItem} roles={props.roles} />;
      default:
        return <TextInput id={fieldId} value={str(props.value)} onChange={(event) => void props.onChange(event.currentTarget.value)} />;
    }
  };
  return (
    <div class="cat-field" classList={{ compact: props.compact, wide: props.spec.type === "list" || props.spec.type === "multiline" }}>
      <label for={fieldId}>{label()}</label>
      {input()}
      <Show when={props.spec.hint && !props.compact}><span class="cat-hint">{tr(props.spec.hint)}</span></Show>
    </div>
  );
}

/** A list field's rows (roster, task skeleton, stages, models): add, remove, reorder. */
function Rows(props: { editor: CatalogEditorStore; spec: CatalogFieldSpec; value: Item[]; onChange: (value: unknown) => Promise<boolean> | void; roleItem?: Item; roles?: RoleContext }) {
  const fields = () => props.spec.fields ?? [];
  const blank = (): Item => Object.fromEntries(fields().map((field) => [field.name, field.type === "number" ? field.min ?? 0 : field.type === "bool" ? false : field.type === "words" ? [] : field.type === "choice" ? field.options?.[0]?.[0] ?? "" : ""]));
  const replace = (index: number, row: Item | undefined) => {
    const rows = [...props.value];
    if (row) rows[index] = row; else rows.splice(index, 1);
    return props.onChange(rows);
  };
  const move = (index: number, by: number) => {
    const rows = [...props.value], target = index + by;
    if (target < 0 || target >= rows.length) return;
    [rows[index], rows[target]] = [rows[target], rows[index]];
    void props.onChange(rows);
  };
  return (
    <div class="cat-rows">
      <Index each={props.value}>{(row, index) => (
        <div class="cat-rowedit">
          <span class="cat-rownum">{index + 1}</span>
          <For each={fields()}>{(field) => (
            <Field editor={props.editor} spec={field} value={row()[field.name]} compact roleItem={props.spec.name === "models" ? props.roleItem : undefined} roles={props.roles} row={row()}
              onChange={(value) => replace(index, { ...row(), [field.name]: value })} />
          )}</For>
          <button type="button" class="icon-btn small" aria-label={tr("Move up")} disabled={index === 0} onClick={() => move(index, -1)}><Icon name="arrow-up" size={12} /></button>
          <button type="button" class="icon-btn small" aria-label={tr("Move down")} disabled={index === props.value.length - 1} onClick={() => move(index, 1)}><Icon name="arrow-down" size={12} /></button>
          <button type="button" class="icon-btn small" aria-label={tr("Remove")} onClick={() => void replace(index, undefined)}><Icon name="x" size={12} /></button>
        </div>
      )}</Index>
      <button type="button" class="btn ghost small" onClick={() => void props.onChange([...props.value, blank()])}>{tr("+ Add")}</button>
    </div>
  );
}

/** Routing: the rules in order, each its own row, and Try it (`hgs team catalog route`). */
function Routing(props: { editor: CatalogEditorStore; set: (kind: CatalogKind, id: string, field: string, value: unknown) => Promise<boolean>; add: () => Promise<void> }) {
  const rules = () => (props.editor.view()?.routing ?? []) as Item[];
  const specs = () => props.editor.schema()?.routing ?? [];
  const [goal, setGoal] = createSignal("");
  const [answer, setAnswer] = createSignal<string>("");
  const tryIt = async () => {
    try {
      const routed = await props.editor.route(goal());
      const template = routed.template as Item | null | undefined;
      const added = Array.isArray(routed.added) ? (routed.added as unknown[]).map(str) : [];
      let text = template ? tr("Uses %1", str(template.name) || str(template.id)) : tr("No template matches; the lead plans.");
      if (routed.rule) text += " " + tr("(rule %1)", str(routed.rule));
      if (added.length) text += " " + tr("Adds %1", added.join(", "));
      setAnswer(text);
    } catch (error) {
      setAnswer(error instanceof Error ? error.message : String(error));
    }
  };
  return (
    <div class="cat-routing">
      <p class="cat-hint">{tr("Rules run top to bottom; the first that matches the goal picks the template.")}</p>
      <For each={rules()}>{(rule, index) => (
        <div class="cat-rule">
          <For each={specs()}>{(spec) => (
            <Field editor={props.editor} spec={spec} value={rule[spec.name]} compact onChange={(value) => props.set("rule", str(rule.id), spec.name, value)} />
          )}</For>
          <button type="button" class="icon-btn small" aria-label={tr("Move up")} disabled={index() === 0} onClick={() => void props.editor.edit({ op: "move", kind: "rule", id: str(rule.id), by: -1 })}><Icon name="arrow-up" size={12} /></button>
          <button type="button" class="icon-btn small" aria-label={tr("Move down")} disabled={index() === rules().length - 1} onClick={() => void props.editor.edit({ op: "move", kind: "rule", id: str(rule.id), by: 1 })}><Icon name="arrow-down" size={12} /></button>
          <button type="button" class="icon-btn small" aria-label={tr("Delete rule")} onClick={() => void props.editor.remove("rule", str(rule.id))}><Icon name="x" size={12} /></button>
        </div>
      )}</For>
      <button type="button" class="btn ghost small cat-add" disabled={props.editor.busy()} onClick={() => void props.add()}>{tr("+ Rule")}</button>
      <div class="cat-try">
        <label for="cat-try-goal">{tr("Try it")}</label>
        <TextInput id="cat-try-goal" value={goal()} placeholder={tr("A goal or issue title…")} onInput={(event) => setGoal(event.currentTarget.value)}
          onKeyDown={(event) => { if (event.key === "Enter") void tryIt(); }} />
        <button type="button" class="btn small" disabled={!goal().trim()} onClick={() => void tryIt()}>{tr("Try")}</button>
        <Show when={answer()}><span class="cat-try-answer" role="status">{answer()}</span></Show>
      </div>
      <p class="cat-hint">{tr("Try it reads the saved catalog; save your edits first to try them.")}</p>
    </div>
  );
}

/** U28: the lead's planning rules as one text; Reset brings the built-in ones back. */
function PlanningRules(props: { editor: CatalogEditorStore }) {
  const dirty = () => (props.editor.view()?.unsaved ?? 0) > 0;
  return (
    <div class="cat-rules">
      <label for="cat-planning-rules">{tr("Planning rules")}</label>
      <span class="cat-hint">{tr("Every lead plans by these rules; they are sent with its brief.")}</span>
      <TextArea id="cat-planning-rules" rows={16} value={props.editor.view()?.planning_rules ?? ""}
        onChange={(event) => void props.editor.edit({ op: "planning_rules", text: event.currentTarget.value })} />
      <div class="cat-rules-foot">
        <span class="grow" />
        <button type="button" class="btn ghost small" disabled={props.editor.busy() || dirty() || !!props.editor.view()?.builtin}
          title={dirty() ? tr("Save or revert your edits first") : tr("Replace the rules with Zerus's built-in ones and save the catalog")}
          onClick={() => void props.editor.resetRules()}>{tr("Reset planning rules")}</button>
      </div>
    </div>
  );
}
