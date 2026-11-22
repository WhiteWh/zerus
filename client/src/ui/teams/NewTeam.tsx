import { For, Show, createEffect, createMemo, createResource, createSignal, onCleanup, onMount } from "solid-js";
import type { JSX } from "solid-js";
import { tr } from "../../i18n";
import { Icon } from "../lib/icons";
import { Spinner } from "../components/Status";
import { pickLocalFolder, projectMemory } from "../../bridge";
import { memberRow, showTeam, teams } from "../../store/teams";
import { followDraft } from "../../store/newTeamFollow";
import { memberRowErrors, type ComputerChoice, type NewTeamStore } from "../../store/teamsNew";
import {
  FOLDER_POLICIES, applyPolicy, branchHint, canAdd, createReason, effectivePolicy, folderPolicy, leadMember, maxMembers, memberOf,
  MEMORY_LABELS, defaultMemory, memberRoles, memoryMode, memoryModes, pickTemplate, startReason, templateTasks, withMemory, type FolderPolicy, type LeadChoice, type MemoryMode,
} from "../../store/newTeamForm";
import { setSetting, settings as settingsStore } from "../../store/settings";
import { shownTemplates, templateSeats, templateTaskCount, toggledFavourite, coordinatorLine, type AccountChoice } from "../../store/newTeamSources";
import { createModelCatalogs, effortOptions, modelOptions } from "./catalogControls";
import { normalizeModelCatalog, selectModel } from "../provider-selection";
import { LeadConversation } from "./TeamTab";
import { NewTeamFooter } from "./NewTeamFooter";
import { TeamHelpButton } from "./TeamHelpButton";
import { NewTeamHowTo, NewTeamIntroHint, NewTeamIntroSteps, PlanModeField, SettingOffers } from "./NewTeamGuide";
import { teamRoleModels } from "../../bridge";
import type { Catalog, CatalogTemplate } from "./catalog";
import type { DraftMember } from "./types";
import type { ComputerId } from "../../types";
import "../styles/newteam.css";
import { TextInput, TextArea } from "../lib/TextField";

// New team (Qt TeamNewDialog/TeamNewTab, task 42). Opening it creates
// nothing; Start planning creates only the draft and its lead; Create team
// creates the members (and their worktrees). The owner's edits save to the
// draft shortly after the last one; the lead's `hgs team draft` edits show
// in the open form, except the fields the owner is changing.

export interface NewTeamProps {
  store: NewTeamStore;
  computer?: ComputerId;
  computers: ComputerChoice[];
  /** The folder is a Git repository (members may have worktrees). */
  git: boolean;
  /** The folder exists (the host checks it); default: a folder is typed. */
  folderOk?: boolean;
  /** Task 200 (Qt PathProbe): the folder check has not answered yet; Start planning waits. */
  folderChecking?: boolean;
  catalog?: Catalog;
  /** The session New team was opened from, which may lead ("This session"). */
  coordinator?: { name: string; agent: string; model?: string; account?: string; busy?: boolean };
  /** This computer's agent accounts, for the lead and the member rows. */
  accounts?: AccountChoice[];
  /** Task 200 (Qt m_accounts[host]): another computer's accounts, for a member placed there. */
  accountsFor?: (computer: ComputerId) => AccountChoice[];
  /** Project of the folder, sent with Start planning. */
  project?: string;
  projectId?: string;
  onOpenSettings?: () => void;
  onAskDesigner?: () => void;
  onCreated?: (team: string) => void;
  onDiscarded?: (team: string) => void;
}

const AGENTS = ["claude", "codex", "kimi", "qwen", "dsh", "opencode"];
const PERMISSIONS: [string, () => string][] = [["", () => tr("Account")], ["auto", () => tr("Auto")], ["bypass", () => tr("Bypass")]];

const FAVOURITES = "teams/templateFavourites";
// Task #8 (team ede724c4): the how-to's collapse, remembered; open until the owner folds it.
const HOWTO_CLOSED = "client/newTeamHowToClosed";
const favourites = () => { const stored = settingsStore[FAVOURITES]; return Array.isArray(stored) ? stored.filter((id): id is string => typeof id === "string") : []; };

/**
 * Task 200 (Qt TeamTemplateList, F6): many templates in a compact list. A
 * search (name, description and the roles a template seats), favourites first
 * with a star and a favourites-only filter, one line per template with its
 * size, and "No template" last; the picked template's roles show under it.
 */
function TemplateList(props: { catalog?: Catalog; value: string; onPick: (id: string) => void }) {
  const [query, setQuery] = createSignal("");
  const [onlyFavourites, setOnlyFavourites] = createSignal(false);
  const shown = createMemo(() => shownTemplates(props.catalog, query(), favourites(), onlyFavourites()));
  const star = (id: string) => void setSetting(FAVOURITES, toggledFavourite(favourites(), id));
  const meta = (template: CatalogTemplate) => {
    const seats = templateSeats(template), tasks = templateTaskCount(template);
    const members = seats === 1 ? tr("1 member") : tr("%1 members", seats);
    return tasks ? tr("%1, %2", members, tasks === 1 ? tr("1 task") : tr("%1 tasks", tasks)) : members;
  };
  const picked = () => props.catalog?.templates.find((template) => template.id === props.value);
  const roleName = (id: string) => props.catalog?.roles.find((role) => role.id === id)?.name ?? id;
  return (
    <div class="nt-templates">
      <div class="nt-inline">
        <TextInput class="grow nt-template-search" type="search" value={query()} placeholder={tr("Search templates or roles…")} aria-label={tr("Search templates or roles…")}
          onInput={(event) => setQuery(event.currentTarget.value)} />
        <button type="button" class="nt-star-filter" classList={{ on: onlyFavourites() }} aria-pressed={onlyFavourites()} title={tr("Show favourites only")}
          aria-label={tr("Show favourites only")} onClick={() => setOnlyFavourites(!onlyFavourites())}>★</button>
      </div>
      <div class="nt-template-list" role="listbox" aria-label={tr("Template")}>
        <For each={shown()}>{(template) => {
          const favourite = () => favourites().includes(template.id);
          return (
            <div class="nt-template-row" role="option" aria-selected={props.value === template.id} data-template={template.id} title={template.description || template.name}
              classList={{ on: props.value === template.id }} onClick={() => props.onPick(template.id)}>
              <span class="nt-template-name">{template.name}</span>
              <span class="nt-hint">{meta(template)}</span>
              <span class="grow" />
              <button type="button" class="nt-star" classList={{ on: favourite() }} data-favourite={template.id}
                title={favourite() ? tr("Remove from favourites") : tr("Add to favourites")} aria-label={favourite() ? tr("Remove from favourites") : tr("Add to favourites")}
                onClick={(event) => { event.stopPropagation(); star(template.id); }}>{favourite() ? "★" : "☆"}</button>
            </div>
          );
        }}</For>
        <div class="nt-template-row" role="option" aria-selected={props.value === "-"} data-template="-" title={tr("Start empty: the lead drafts members and tasks from the goal")}
          classList={{ on: props.value === "-" }} onClick={() => props.onPick("-")}>
          <span class="nt-template-name">{tr("No template")}</span><span class="nt-hint">{tr("the lead drafts the team")}</span>
        </div>
      </div>
      <p class="nt-hint nt-template-detail">{picked()
        ? tr("%1: %2", picked()!.name, picked()!.roster.length ? picked()!.roster.map((seat) => (seat.count > 1 ? tr("%1 x%2", roleName(seat.role), seat.count) : roleName(seat.role))).join(", ") : tr("no members"))
        : tr("No template: the lead drafts members and tasks from the goal.")}</p>
    </div>
  );
}

/** Discard draft's question (Qt teamNewDiscard): Cancel focused; it may also stop the lead's session. */
function DiscardDialog(props: { name: string; close: () => void; discard: (stopLead: boolean) => void }) {
  let dialog!: HTMLDialogElement;
  const [stopLead, setStopLead] = createSignal(false);
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLElement>("[data-initial]")?.focus()); });
  onCleanup(() => { if (dialog.open) dialog.close(); });
  return (
    <dialog ref={dialog} class="dlg" aria-labelledby="nt-discard-title" onCancel={(event) => { event.preventDefault(); props.close(); }}>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} /><h2 id="nt-discard-title">{tr("Discard draft")}</h2></div>
      <div class="dlg-body">
        <p>{tr("Discard the draft “%1”? Nothing of it is created.", props.name || tr("New team"))}</p>
        <label class="nt-check"><input type="checkbox" checked={stopLead()} onChange={(event) => setStopLead(event.currentTarget.checked)} />{tr("Also stop the lead's session")}</label>
      </div>
      <div class="dlg-foot"><span class="spacer" />
        <button type="button" class="btn solid-danger" onClick={() => { props.discard(stopLead()); props.close(); }}>{tr("Discard draft")}</button>
        <button type="button" class="btn primary" data-initial autofocus onClick={props.close}>{tr("Cancel")}</button>
      </div>
    </dialog>
  );
}

export function NewTeam(props: NewTeamProps): JSX.Element {
  const store = props.store;
  const computer = () => props.computer ?? "";
  const fields = () => store.fields();
  const draft = () => store.draftId();
  const folderOk = () => props.folderOk ?? fields().folder.trim() !== "";
  const settings = () => fields().settings ?? {};
  const [template, setTemplate] = createSignal("-");
  const [planning, setPlanning] = createSignal("lead");
  const [showProtocol, setShowProtocol] = createSignal(false);
  const [addOpen, setAddOpen] = createSignal(false);
  const [discarding, setDiscarding] = createSignal(false);
  const [starting, setStarting] = createSignal(false);
  const leadRole = () => props.catalog?.roles.find((role) => role.id === "lead");
  const [lead, setLead] = createSignal<LeadChoice>({
    mode: props.coordinator?.name ? "this" : "new",
    session: props.coordinator?.name ? { name: props.coordinator.name, agent: props.coordinator.agent } : null,
    agent: leadRole()?.agent || "claude", model: leadRole()?.model ?? "", effort: "", account: "",
  });

  const policy = createMemo(() => effectivePolicy(folderPolicy(settings()), fields().folder, props.git));
  // Task 126: a project with RustCodeLore on gives the team a shared
  // workspace in its store; otherwise New team offers the memory mode,
  // RustCodeLore by default (written into the settings, so the draft has it).
  const [projectMemoryOn] = createResource(() => props.projectId || undefined, async (project) => {
    try { const outcome = await projectMemory({ project, op: { op: "read" } }); return outcome.outcome === "done" && outcome.view.enabled; }
    catch { return false; }
  });
  // Task 127: the preselected mode follows the project's memory until the
  // owner chooses one: project when it is on, team when it is off.
  const [memoryChosen, setMemoryChosen] = createSignal(false);
  createEffect(() => {
    if (draft() || memoryChosen() || projectMemoryOn.loading) return;
    const wanted = defaultMemory(!!projectMemoryOn());
    if ((settings().memory as { mode?: unknown } | undefined)?.mode !== wanted) store.edit("settings", withMemory(settings(), wanted));
  });
  const chooseMemory = (mode: MemoryMode) => { setMemoryChosen(true); store.edit("settings", withMemory(settings(), mode)); };
  const members = () => fields().members;
  const unseen = (field: string) => store.unseen().includes(field);
  const mark = (field: string) => (
    <Show when={unseen(field)}>
      <button type="button" class="nt-unseen" title={tr("Changed by the lead, not yet seen: click when you have seen it")} onClick={() => void store.markSeen(field)}>●</button>
    </Show>
  );

  // The lead's edits reach the open form; the owner's edits save shortly after the last one.
  followDraft({ computer, draft, show: showTeam, view: (c, id) => teams.newTeam(c, id), apply: store.applyView });
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    if (!draft() || store.dirty().size === 0) return;
    clearTimeout(timer);
    timer = setTimeout(() => void store.flush(), 600);
  });
  onCleanup(() => clearTimeout(timer));

  const setSettings = (patch: Record<string, unknown>) => {
    const next = { ...settings(), ...patch };
    for (const [key, value] of Object.entries(patch)) if (value === undefined) delete next[key];
    store.edit("settings", next);
  };
  const setMembers = (next: DraftMember[]) => store.edit("members", next);
  const setMember = (index: number, patch: Partial<DraftMember>) => setMembers(members().map((member, at) => (at === index ? { ...member, ...patch } : member)));
  const choosePolicy = (value: FolderPolicy) => {
    store.editMany({ settings: { ...settings(), folder_policy: value }, members: applyPolicy(members(), value, props.git) });
  };
  const chooseTemplate = (id: string) => {
    setTemplate(id);
    const picked = pickTemplate(props.catalog, id, { settings: settings(), addendum: fields().addendum, addendumEdited: store.addendumEdited() },
      props.git && policy().policy !== "shared");
    setPlanning(picked.planning);
    store.editMany({ members: picked.members, settings: picked.settings, addendum: picked.addendum,
      tasks: templateTasks(props.catalog, id, picked.planning, picked.members) });
  };
  const choosePlanning = (value: string) => {
    setPlanning(value);
    store.edit("tasks", templateTasks(props.catalog, template(), value, members()));
  };
  const add = (roleId: string | null) => {
    setAddOpen(false);
    const role = roleId ? props.catalog?.roles.find((found) => found.id === roleId) : undefined;
    setMembers([...members(), memberOf(role, props.git && policy().policy !== "shared")]);
  };
  const browse = async () => {
    try {
      const picked = await pickLocalFolder(fields().folder);
      if (picked) store.setFolder(picked);
    } catch { /* the dialog is not available here: the field stays typed */ }
  };
  const start = async () => {
    if (startWhy()) return;
    setStarting(true);
    try {
      await store.startPlanning({
        computer: computer(), folder: fields().folder.trim(), name: fields().name.trim() || undefined,
        lead: leadMember(lead(), leadRole()?.prompt ?? ""), project: props.project, project_id: props.projectId,
        template: template() !== "-" && props.catalog?.templates.some((found) => found.id === template()) ? template() : undefined,
      });
    } finally { setStarting(false); }
  };
  const create = async (confirmWidening = false) => {
    const id = draft();
    const result = await store.createTeam({ confirmWidening });
    if (result.ok) props.onCreated?.(id);
  };
  const discard = async (stopLead: boolean) => {
    const id = draft();
    const result = await store.discard(stopLead);
    if (result.ok) props.onDiscarded?.(id);
  };

  // WhiteWh fork: a form for another computer cannot judge that computer's
  // aliases; its hgs checks them, and the preflight says so.
  const rowErrors = (member: DraftMember) => memberRowErrors(member, computer() ? undefined : props.computers);
  const badRows = () => members().filter((member) => rowErrors(member).length > 0).length;
  const counted = () => members().filter((member) => !member.lead && member.id !== "lead").length;
  const createWhy = () => createReason({ drafting: !!draft(), goal: fields().goal, name: fields().name, folderOk: folderOk(), badRows: badRows(),
    // Owner, 2026-11-09: the limit counts the members besides the lead, as hgs does; counting the lead blocked a valid team.
    members: counted(), max: maxMembers(settings()), preflightErrors: store.view()?.validation.errors.length ?? 0 });
  const startWhy = () => (fields().folder.trim() && props.folderChecking ? tr("Checking the folder…") : startReason(fields().folder, folderOk(), lead()));
  // Task 200 (Qt m_accounts[host]): a member's accounts are those of its computer.
  // WhiteWh fork (owner, 2026-11-18): a team made on another computer lives
  // there; "" in its rows is that computer, whose accounts and models apply.
  // A row placed on a third computer names it by the other computer's alias,
  // which this window cannot ask: no accounts or models are offered for it.
  const placed = <T extends { computer: string }>(item: T): T => ({ ...item, computer: item.computer || computer() });
  const reachable = (item: { computer: string }) => !computer() || !item.computer;
  const accountsFor = (agent: string, at: ComputerId = "") => {
    if (!reachable({ computer: at })) return [];
    const where = at || computer();
    return (where && props.accountsFor ? props.accountsFor(where) : props.accounts ?? []).filter((account) => account.provider === agent);
  };
  // Task 200 (Qt task #19): the models an agent's account offers on that computer, as the model field's list.
  const [modelTick, setModelTick] = createSignal(0);
  const models = createModelCatalogs((at, agent, account) => teamRoleModels(at, agent, account), () => setModelTick((n) => n + 1));
  interface ModelItem { [key: string]: unknown; computer: string; agent: string; account: string; model: string; effort: string }
  const catalogFor = (shown: ModelItem) => {
    if (!reachable(shown)) return undefined;
    const item = placed(shown);
    modelTick();
    models.request(item);
    return models.get(item);
  };
  const modelsFor = (shown: ModelItem) => modelOptions(placed(shown), catalogFor(shown));
  const effortsFor = (shown: ModelItem) => {
    const item = placed(shown);
    return effortOptions(item.model, catalogFor(shown), item.effort, item.agent);
  };
  const changedModel = (shown: ModelItem, value: string) => {
    const item = placed(shown);
    const next = selectModel(normalizeModelCatalog((reachable(shown) && models.get(item)) || []), value, item.effort);
    return { model: next.modelId, effort: next.effortId };
  };
  const leadModelInput = (value: string) => setLead({ ...lead(), ...changedModel({ computer: "", ...lead(), model: value }, value) });
  const memberModelInput = (index: number, value: string) => {
    const item = members()[index];
    setMember(index, changedModel({ ...item, model: value }, value));
  };
  // Task 200 (Qt buildLead): the lead pane's Chat, Goal and Plan.
  const [leadPage, setLeadPage] = createSignal<"chat" | "goal" | "phases" | "plan">("chat");
  const draftLead = () => store.view()?.members.find((member) => member.lead);
  const coordinatorText = () => props.coordinator ? coordinatorLine(props.coordinator, props.accounts ?? []) : "";

  // The draft's phases (the view's records, else the tasks' phase numbers).
  const draftPhases = createMemo(() => {
    const records = store.view()?.phases ?? [];
    const tasks = fields().tasks;
    const numbers = [...new Set([...records.map((p) => p.n), ...tasks.map((t) => t.phase).filter((n) => n > 0)])].sort((a, b) => a - b);
    return numbers.map((n) => {
      const record = records.find((p) => p.n === n);
      return {
        n,
        title: record?.title ?? "",
        gate: !!record?.barrier,
        owner: !!(record?.owner_accepts || record?.acceptance === "owner"),
        mode: record?.mode ?? "",
        goal: (record?.note ?? "").trim(),
        tasks: tasks.filter((t) => t.phase === n),
      };
    });
  });
  const leadPicker = () => (
            <fieldset class="nt-lead">
              <legend>{tr("Who leads the team? The chat here is with the lead: it drafts the team with you.")}</legend>
              <label class="nt-check" title={props.coordinator?.name ?? ""}>
                <input type="radio" name="nt-lead" disabled={!props.coordinator?.name} checked={lead().mode === "this"} onChange={() => setLead({ ...lead(), mode: "this" })} />
                {tr("This session")}<Show when={props.coordinator?.name}><span class="nt-hint">{props.coordinator!.name}</span></Show>
              </label>
              <Show when={lead().mode === "this" && props.coordinator?.busy}>
                <p class="nt-hint">{tr("This session is working. The draft's brief waits until it is free; it is queued once, never sent twice.")}</p>
              </Show>
              <label class="nt-check"><input type="radio" name="nt-lead" checked={lead().mode === "new"} onChange={() => setLead({ ...lead(), mode: "new" })} />{tr("A new session in the team folder")}</label>
              <div class="nt-row nt-lead-row">
                <select value={lead().agent} disabled={lead().mode !== "new"} aria-label={tr("Agent")} onChange={(event) => setLead({ ...lead(), agent: event.currentTarget.value, account: "", model: "", effort: "" })}>
                  <For each={AGENTS}>{(agent) => <option value={agent}>{agent}</option>}</For>
                </select>
                <TextInput placeholder={tr("default model")} value={lead().model} disabled={lead().mode !== "new"} list="nt-lead-models" onInput={(event) => leadModelInput(event.currentTarget.value)} />
                <datalist id="nt-lead-models"><For each={modelsFor({ computer: "", agent: lead().agent, account: lead().account, model: lead().model, effort: lead().effort })}>{(option) => <option value={option.id}>{option.label}</option>}</For></datalist>
                <select value={lead().effort} disabled={lead().mode !== "new"} aria-label={tr("Effort")} onChange={(event) => setLead({ ...lead(), effort: event.currentTarget.value })}>
                  <For each={effortsFor({ computer: "", agent: lead().agent, account: lead().account, model: lead().model, effort: lead().effort })}>{(effort) => <option value={effort}>{effort || tr("Default effort")}</option>}</For>
                </select>
                <select value={lead().account} disabled={lead().mode !== "new"} aria-label={tr("Account")} onChange={(event) => setLead({ ...lead(), account: event.currentTarget.value, model: "", effort: "" })}>
                  <option value="">{tr("Default account")}</option>
                  <For each={accountsFor(lead().agent)}>{(account) => <option value={account.id} selected={account.id === lead().account}>{account.signedIn ? account.label || account.id : tr("%1 (signed out)", account.label || account.id)}</option>}</For>
                </select>
              </div>
            </fieldset>
  );
  // Owner, 2026-11-09: before a team exists, the New team tab explains how a
  // team works and asks only who leads it; the full form (template, members,
  // planning, memory) shows once the lead has started the draft.
  const intro = () => (
    <div class="nt-intro" data-new-team-intro>
      <NewTeamIntroSteps />
      <div class="nt-intro-lead">
        <div class="nt-field">
          <span>{tr("Folder")}</span>
          <div class="nt-inline">
            <TextInput class="grow" value={fields().folder} onInput={(event) => store.setFolder(event.currentTarget.value)} />
            <button type="button" class="btn small" onClick={() => void browse()}>{tr("Browse…")}</button>
          </div>
        </div>
        {leadPicker()}
        <button type="button" class="btn primary nt-intro-start" data-start-planning disabled={store.busy() || starting() || !!startWhy()} title={startWhy()} onClick={() => void start()}>
          <Show when={store.busy() || starting()}><Spinner size={14} /> </Show>{tr("Start working with a team")}
        </button>
        <NewTeamIntroHint />
        <Show when={store.error()}><p class="nt-error nt-block-error">{store.error()}</p></Show>
      </div>
    </div>
  );
  return (
    <section class="nt" aria-label={tr("New team")}>
      <header class="nt-head">
        <h2>{tr("New team")}</h2>
        <span class="grow" />
        <Show when={props.onAskDesigner && !computer()}>
          <button type="button" class="btn ghost small" title={tr("The Team designer keeps the templates and roles; the lead plans this team with you")}
            onClick={() => props.onAskDesigner?.()}>{tr("Templates and roles: Team designer")}</button>
        </Show>
        <Show when={draft() && props.onOpenSettings}>
          <button type="button" class="btn ghost small" onClick={() => props.onOpenSettings?.()}>{tr("Team settings")}</button>
        </Show>
        <TeamHelpButton />
        <button type="button" class="icon-btn small" aria-label={tr("Close")} onClick={store.close}><Icon name="x" size={14} /></button>
      </header>

      <Show when={draft()} fallback={intro()}>
      <div class="nt-split">
      <div class="nt-body">
        <NewTeamHowTo open={settingsStore[HOWTO_CLOSED] !== true} onToggle={(open) => void setSetting(HOWTO_CLOSED, !open)} />
        <div class="nt-field">
          <span>{tr("Template")}</span>
          <TemplateList catalog={props.catalog} value={template()} onPick={chooseTemplate} />
        </div>
        <label class="nt-field">
          <span>{tr("Name")} {mark("name")}</span>
          <TextInput value={fields().name} maxlength={80} placeholder={tr("Checkout redesign")} onInput={(event) => store.edit("name", event.currentTarget.value)} />
        </label>
        <div class="nt-field">
          <span>{tr("Folder")}</span>
          <div class="nt-inline">
            <TextInput class="grow" value={fields().folder} disabled={!!draft()} onInput={(event) => store.setFolder(event.currentTarget.value)} />
            <button type="button" class="btn small" disabled={!!draft()} onClick={() => void browse()}>{tr("Browse…")}</button>
          </div>
        </div>
        <div class="nt-field">
          <span>{tr("Folders")} {mark("settings")}</span>
          <div class="nt-inline">
            <select class="nt-policy" value={policy().policy} disabled={policy().forced} onChange={(event) => choosePolicy(event.currentTarget.value as FolderPolicy)}
              title={policy().forced ? tr("Folder is not a Git repository: every member works in this folder, so the policy is shared.") : tr("Auto lets the lead choose the folder each member works in")}>
              <For each={FOLDER_POLICIES}>{(choice) => <option value={choice.value}>{choice.label()}</option>}</For>
            </select>
            <span class="nt-hint">{branchHint(fields().name, props.git)}</span>
          </div>
        </div>
        <SettingOffers offers={store.view()?.draft.setting_offers}
          onApply={(field, value) => setSettings({ [field]: value === null ? undefined : value })} />
        <Show when={fields().folder.trim() && !props.git}>
          <p class="nt-note">{tr("This folder is not a Git repository. Every member will share it; worktrees are off.")}</p>
        </Show>

        <Show when={!draft() || store.view()?.working_mode?.proposal}>
          <section class="nt-working-mode" data-working-mode={store.view()?.working_mode?.proposal?.preset ?? "auto"}>
            <div class="nt-inline">
              <b>{tr("Working mode")}</b>
              <Show when={store.view()?.working_mode?.proposal}>
                {(proposal) => <span class="nt-hint">{tr("The project suggests %1", proposal().title || proposal().preset)}</span>}
              </Show>
            </div>
            <Show when={store.view()?.working_mode?.proposal} fallback={<span class="nt-hint">{tr("Auto: follow the project")}</span>}>
              {(proposal) => (
                <>
                  <label class="nt-field">
                    <span>{tr("How members work")} {mark("working_mode")}</span>
                    <select value={fields().working_mode} onChange={(event) => store.edit("working_mode", event.currentTarget.value)}>
                      <option value="auto">{tr("Auto: follow the project (%1)", proposal().preset)}</option>
                      <option value="branch-pipeline">{tr("Branch pipeline")}</option>
                      <option value="branch-per-task">{tr("Branch per task")}</option>
                      <option value="shared-folder">{tr("One shared folder")}</option>
                      <option value="none">{tr("Keep the settings as they are")}</option>
                    </select>
                  </label>
                  <ul class="nt-working-reasons">
                    <For each={proposal().reasons}>{(reason) => <li>{reason}</li>}</For>
                  </ul>
                </>
              )}
            </Show>
          </section>
        </Show>

        <PlanModeField mode={fields().plan_mode} offer={store.view()?.plan_mode_offer} reason={store.view()?.plan_mode_reason}
          unseen={unseen("plan_mode") || unseen("plan_mode_reason")} onChoose={(mode) => store.edit("plan_mode", mode)}
          onSeen={() => void store.markSeen(["plan_mode", "plan_mode_reason"])} />

        <div class="nt-members">
          <div class="nt-members-head">
            <b>{tr("Members")}</b>
            <span class="nt-hint" title={tr("The limit counts the members besides the lead")}>{tr("%1 / %2", counted(), maxMembers(settings()))}</span>
            {mark("members")}
            <span class="grow" />
            <div class="nt-add">
              <button type="button" class="btn small" aria-haspopup="menu" aria-expanded={addOpen()} disabled={!canAdd(members(), settings())}
                onClick={() => setAddOpen(!addOpen())}>{tr("+ Member")}</button>
              <Show when={addOpen()}>
                <div class="menu nt-add-menu" role="menu" aria-label={tr("Add a member")}>
                  <For each={memberRoles(props.catalog)}>{(role) => (
                    <button type="button" role="menuitem" class="menu-item" onClick={() => add(role.id)}>
                      {role.model ? tr("%1, %2, %3", role.name, role.agent, role.model) : tr("%1, %2", role.name, role.agent)}
                    </button>
                  )}</For>
                  <button type="button" role="menuitem" class="menu-item" onClick={() => add(null)}>{tr("Member without a role")}</button>
                </div>
              </Show>
            </div>
          </div>
          <Show when={members().length === 0}>
            <p class="nt-hint">{draft() ? tr("No members yet. The lead drafts them with you, or add them by role.") : tr("No members yet. Pick a template, add members by role, or let the lead draft them.")}</p>
          </Show>
          <For each={members()}>{(member, index) => (
            <div class="nt-row nt-member">
              <TextInput class="nt-role" placeholder={tr("Role")} value={member.role} onInput={(event) => setMember(index(), { role: event.currentTarget.value })} />
              <select class="nt-agent" value={member.agent} onChange={(event) => setMember(index(), { agent: event.currentTarget.value, account: "", model: "", effort: "" })}>
                <For each={AGENTS}>{(agent) => <option value={agent}>{agent}</option>}</For>
              </select>
              <TextInput class="nt-model" placeholder={tr("default model")} value={member.model} list={`nt-models-${index()}`} onInput={(event) => memberModelInput(index(), event.currentTarget.value)} />
              <datalist id={`nt-models-${index()}`}><For each={modelsFor({ ...member })}>{(option) => <option value={option.id}>{option.label}</option>}</For></datalist>
              <select class="nt-effort" value={member.effort} aria-label={tr("Effort")} onChange={(event) => setMember(index(), { effort: event.currentTarget.value })}>
                <For each={effortsFor({ ...member })}>{(effort) => <option value={effort}>{effort || tr("Default effort")}</option>}</For>
              </select>
              <select class="nt-permissions" value={member.permissions} aria-label={tr("Permissions")} onChange={(event) => setMember(index(), { permissions: event.currentTarget.value })}>
                <For each={PERMISSIONS}>{([value, label]) => <option value={value}>{label()}</option>}</For>
              </select>
              <select class="nt-computer" value={member.computer} aria-label={tr("Computer")} onChange={(event) => setMember(index(), { computer: event.currentTarget.value, account: "", model: "", effort: "" })}>
                <option value="">{computer() ? props.computers.find((choice) => choice.alias === computer())?.label || computer() : tr("this computer")}</option>
                {/* A team on another computer names its members' computers by that computer's aliases, which this one does not know. */}
                <For each={computer() ? (member.computer ? [{ alias: member.computer, label: member.computer }] : []) : props.computers.filter((choice) => choice.alias)}>{(choice) => <option value={choice.alias}>{choice.label}</option>}</For>
              </select>
              <select class="nt-account" value={member.account} aria-label={tr("Account")} onChange={(event) => setMember(index(), { account: event.currentTarget.value, model: "", effort: "" })}>
                <option value="" selected={!member.account}>{tr("Default account")}</option>
                <For each={accountsFor(member.agent, member.computer)}>{(account) => <option value={account.id} selected={account.id === member.account}>{account.signedIn ? account.label || account.id : tr("%1 (signed out)", account.label || account.id)}</option>}</For>
              </select>
              <label class="nt-check" title={tr("Own worktree")}>
                <input type="checkbox" checked={member.folder === "worktree"} disabled={!props.git || policy().policy === "shared"}
                  onChange={(event) => setMember(index(), { folder: event.currentTarget.checked ? "worktree" : "shared" })} />{tr("Own worktree")}
              </label>
              <button type="button" class="icon-btn small" aria-label={tr("Duplicate")} title={tr("Duplicate")} disabled={!canAdd(members(), settings())}
                onClick={() => setMembers([...members().slice(0, index() + 1), { ...member, id: "" }, ...members().slice(index() + 1)])}><Icon name="copy" size={13} /></button>
              <button type="button" class="icon-btn small" aria-label={tr("Remove member")} title={tr("Remove member")}
                onClick={() => setMembers(members().filter((_, at) => at !== index()))}><Icon name="x" size={13} /></button>
              <Show when={rowErrors(member).length > 0}>
                <span class="nt-error">{rowErrors(member).map((error) => (error.code === "role" ? tr("Pick a role") : tr("Computer %1 is offline", error.alias))).join(", ")}</span>
              </Show>
            </div>
          )}</For>
        </div>

        <div class="nt-options">
          <div class="nt-field">
            <span>{tr("Planning")}</span>
            <div class="nt-inline" role="radiogroup" aria-label={tr("Planning")}>
              <label class="nt-check"><input type="radio" name="nt-planning" checked={planning() === "lead"} onChange={() => choosePlanning("lead")} />{tr("Lead plans")}</label>
              <label class="nt-check"><input type="radio" name="nt-planning" checked={planning() === "template"} onChange={() => choosePlanning("template")} />{tr("Use template tasks")}</label>
            </div>
          </div>
          <div class="nt-field">
            <span>{tr("Approval")}</span>
            <div class="nt-inline">
              <label class="nt-check"><input type="checkbox" checked={settings().approval !== "auto"} onChange={(event) => setSettings({ approval: event.currentTarget.checked ? "review" : "auto" })} />{tr("Review plan before running")}</label>
              <select value={typeof settings().phase_approval === "string" ? String(settings().phase_approval) : ""} onChange={(event) => setSettings({ phase_approval: event.currentTarget.value || undefined })}>
                <option value="">{tr("Later phases: as the plan")}</option>
                <option value="review">{tr("Later phases: review")}</option>
                <option value="auto">{tr("Later phases: auto")}</option>
              </select>
            </div>
          </div>
          <div class="nt-field">
            <span>{tr("Memory")}</span>
            <div class="nt-inline" role="radiogroup" aria-label={tr("Memory")}>
              <For each={memoryModes(!!projectMemoryOn())}>{(mode) => (
                <label class="nt-check" title={tr(MEMORY_LABELS[mode].hint)}>
                  <input type="radio" name="nt-memory" value={mode} checked={memoryMode(settings(), !!projectMemoryOn()) === mode} onChange={() => chooseMemory(mode)} />
                  {tr(MEMORY_LABELS[mode].label)}
                </label>
              )}</For>
            </div>
            <span class="nt-hint">{tr(MEMORY_LABELS[memoryMode(settings(), !!projectMemoryOn())].hint)}</span>
          </div>
          <label class="nt-field">
            <span>{tr("Max members")}</span>
            <TextInput type="number" min={1} max={32} class="nt-max" value={maxMembers(settings())}
              onInput={(event) => { const n = Math.round(Number(event.currentTarget.value)); if (n >= 1 && n <= 32) setSettings({ max_members: n }); }} />
          </label>
          <button type="button" class="btn ghost small" aria-expanded={showProtocol()} onClick={() => setShowProtocol(!showProtocol())}>{showProtocol() ? tr("Hide protocol addendum") : tr("Protocol addendum")}</button> {mark("addendum")}
          <Show when={showProtocol()}>
            <TextArea rows={4} placeholder={tr("How the lead splits the work, checks reports and starts the next wave. A template fills this in.")}
              value={fields().addendum} onInput={(event) => store.edit("addendum", event.currentTarget.value)} />
          </Show>
        </div>

        <Show when={store.error()}><p class="nt-error nt-block-error">{store.error()}</p></Show>
        <Show when={store.created()}><p class="nt-created">{tr("Team created.")}</p></Show>
        {/* Owner, 2026-11-09: Create team and what blocks it sit under the form. */}
        <NewTeamFooter draft={!!draft()} dirty={store.dirty().size > 0} revision={store.revision()} busy={store.busy()}
          preflightErrors={store.view()?.validation.errors.length} errors={store.view()?.validation.errors} createWhy={createWhy()}
          error={store.error()}
          fix={counted() > maxMembers(settings()) ? { label: tr("Raise the limit to %1", counted()), run: () => setSettings({ max_members: Math.min(32, counted()) }) }
            : /confirm_widening|wider permissions/i.test(store.error()) ? { label: tr("Confirm the wider permissions and create"), run: () => void create(true) } : undefined}
          onUseLead={store.useLeadVersion} onDiscard={() => setDiscarding(true)} onCreate={() => void create()} />
      </div>

      {/* Task 200 (Qt buildLead): the lead's pane, Chat (who leads, then the lead's conversation), Goal and Plan. */}
      <aside class="nt-lead-pane" aria-label={tr("Lead")}>
        <div class="nt-lead-head">
          <b class="nt-caps">{tr("LEAD")}</b>
          <span class="nt-hint ell grow">{draftLead()?.session?.name ?? (draft() ? "" : lead().mode === "this" ? coordinatorText() : tr("A new session in the team folder"))}</span>
          <div class="seg nt-lead-tabs" role="tablist" aria-label={tr("Lead")}>
            <For each={[["chat", tr("Chat")], ["goal", tr("Goal")], ["phases", tr("Phases")], ["plan", tr("Plan")]] as const}>{([id, label]) => (
              <button type="button" role="tab" data-lead-page={id} aria-selected={leadPage() === id} classList={{ on: leadPage() === id }} onClick={() => setLeadPage(id)}>
                {label}{id === "goal" ? mark("goal") : id === "plan" ? mark("tasks") : null}
              </button>
            )}</For>
          </div>
        </div>

        <Show when={leadPage() === "chat"}>
          <Show when={draft()} fallback={
            leadPicker()
          }>
            <Show when={draftLead()?.session && memberRow(computer(), draftLead())} fallback={<p class="nt-hint nt-lead-starting">{tr("The lead's session is starting; its chat shows here once it runs.")}</p>}>
              <LeadConversation computer={computer()} lead={draftLead()} label={tr("Message the lead")} placeholder={tr("Message the lead…")} send={tr("Write to the lead")} />
            </Show>
            <Show when={store.unseen().length > 0}>
              <p class="nt-hint">{tr("● changed by the lead, not yet seen: click a mark when you have seen it")}</p>
            </Show>
          </Show>
        </Show>

        <Show when={leadPage() === "goal"}>
          <label class="nt-field nt-goal">
            <span>{tr("Goal")} {mark("goal")} <span class="nt-hint">{tr("what the team delivers: every member reads it with each task")}</span></span>
            <TextArea rows={10} placeholder={tr("Describe the result, constraints and what done means. The lead may write it with you.")}
              value={fields().goal} onInput={(event) => store.edit("goal", event.currentTarget.value)} />
          </label>
        </Show>

        {/* Owner, 2026-11-09: the draft's phases at a glance - how many, in what
            order, which wait as gates, who accepts each, its goal and its tasks. */}
        <Show when={leadPage() === "phases"}>
          <div class="nt-phases" data-draft-phases>
            <Show when={draftPhases().length > 0} fallback={<p class="nt-hint">{tr("No phases yet. Ask the lead in Chat to split the plan into phases, each with its goal.")}</p>}>
              <p class="nt-hint nt-phases-sum">{tr("%1 phases, %2 gates, %3 accepted by you, %4 tasks", draftPhases().length, draftPhases().filter((p) => p.gate).length, draftPhases().filter((p) => p.owner).length, fields().tasks.length)}</p>
              <ol class="nt-phase-list">
                <For each={draftPhases()}>{(phase) => (
                  <li class="nt-phase" data-phase={phase.n}>
                    <div class="nt-inline">
                      <b>{tr("Phase %1", phase.n)}</b><span class="ell grow">{phase.title}</span>
                      <Show when={phase.gate}><span class="team-pill warn" title={tr("Starts only after every earlier phase is accepted")}>{tr("gate")}</span></Show>
                      <span class="team-pill" title={tr("Who accepts the phase")}>{phase.owner ? tr("you accept") : tr("lead accepts")}</span>
                      <Show when={phase.mode}><span class="nt-hint">{phase.mode}</span></Show>
                      <span class="nt-hint">{tr("%1 tasks", phase.tasks.length)}</span>
                    </div>
                    <p class="nt-phase-goal" classList={{ empty: !phase.goal }}>{phase.goal ? tr("Done when: %1", phase.goal) : tr("No goal yet: ask the lead what this phase delivers and when it is done.")}</p>
                    <Show when={phase.tasks.length > 0}>
                      <ul class="nt-phase-tasks"><For each={phase.tasks}>{(task) => <li><span class="nt-hint">#{task.id}</span> {task.title}</li>}</For></ul>
                    </Show>
                  </li>
                )}</For>
              </ol>
            </Show>
          </div>
        </Show>

        <Show when={leadPage() === "plan"}>
          <div class="nt-plan">
            <div class="nt-inline"><b>{tr("Plan")}</b> {mark("tasks")} <span class="nt-hint">{tr("the lead drafts it; you approve it after Create team")}</span></div>
            <Show when={fields().tasks.length > 0} fallback={<p class="nt-hint" data-plan-empty>{tr("No plan yet. Ask the lead in Chat, or use the template's tasks under Planning.")}</p>}>
              <ol class="nt-plan-list">
                <For each={fields().tasks}>{(task) => (
                  <li data-plan-task={task.id}>
                    <span class="nt-plan-title">{task.title}</span>
                    <Show when={task.assignee}><span class="nt-hint">{task.assignee}</span></Show>
                    <Show when={task.phase}><span class="nt-hint">{tr("phase %1", task.phase)}</span></Show>
                  </li>
                )}</For>
              </ol>
            </Show>
          </div>
        </Show>
      </aside>
      </div>
      </Show>

      <Show when={discarding()}>
        <DiscardDialog name={fields().name.trim()} close={() => setDiscarding(false)} discard={(stopLead) => void discard(stopLead)} />
      </Show>
    </section>
  );
}
