import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from "solid-js";
import { trText } from "../../text";
import * as bridge from "../../bridge";
import type { PathStyle } from "../../types";
import { AgentGlyph, Icon } from "../lib/icons";
import { composing } from "../lib/keys";
import { MachineTile, type MachineState } from "../components/MachineTile";
import { UsageMeters } from "../components/UsageMeter";
import { FolderBrowserDialog } from "../components/FolderBrowser";
import type { FolderBrowserCalls } from "../components/folder-browser";
import { closeNewSession, newSession, newSessionActions as act, startNewSession, type NewSessionState } from "../../store/newSession";
import { machineCards } from "../../store/machines";
import { catalog, agentKind } from "../../store/catalog";
import { connectionOf, osOf } from "../../store/overview";
import { now } from "../../store/ui";
import { projectColor } from "./organization-ui";
import { launchWindows, type AccountRow } from "./newsession-ui";
import { accountKey, computerKey, createConfirm, dialogKey, folderKey, projectKey, type KeyControl } from "./newsession-view";
import { LaunchWarning } from "./LaunchWarning";
import { ModelPopover, type ModelPickerActions } from "./ModelPicker";
import { ChooseWorktreeDialog } from "./WorktreePicker";
import { NewWorktreeDialog } from "./NewWorktreeDialog";
import { sameSessions, type PickerSession } from "./worktree-dialogs";
import type { Json } from "../lib/json";
import { tr } from "../../i18n";
import "../styles/newsession.css";

// New session (tray/src/NewSessionDialog.cpp, constructor 136-475 and
// updateForm 660-742), from the prototype's layout
// (design/webui/src/dialogs/setup/NewSession.tsx) where Qt does not differ.
// It renders the store's state and calls its actions; the form's rules come
// from the core through the store (`form` is `launch::shown`). Opening and
// every choice here only read; Start hands the plan to background creation.
// The child dialogs (a peer's folder browser, Choose existing…, Create
// manually…, the model picker) open over it from `newSessionChild` and the
// store's `pickerOpen`.

/** A child dialog New session opens over itself, with the context it opened in (585, 1114-1143). */
export type NewSessionChild =
  | { kind: "browse"; target: string; computer: string; project: string; path: string }
  | { kind: "browse-repository"; target: string; computer: string; parent: string }
  | { kind: "choose-worktree"; target: string; computer: string; project: string; path: string; style: PathStyle }
  | { kind: "create-worktree"; target: string; computer: string; project: string; path: string; catalog: Json; branch: string; style: PathStyle };
export const [newSessionChild, setNewSessionChild] = createSignal<NewSessionChild>();

const AGENTS = new Set(["codex", "claude", "kimi", "dsh", "opencode"]);
const sameKeys = (a: string[], b: string[]) => a.length === b.length && a.every((key, i) => key === b[i]);

/** The row whose key is `key`, keeping the last one found while a removed row is disposed. */
function rowOf<T>(find: () => T | undefined): () => T {
  return createMemo<T>((last) => find() ?? last, find() as T);
}

/**
 * A choice list with rows that a label cannot carry in a native list (the
 * project colour, the computer's OS tile). Rows are keyed: relabelling keeps
 * them and the hovered one. Its keys are Qt's non-editable combo box
 * (`dialogKey`): on the closed button the arrows, PageUp, PageDown, Home and
 * End change the choice, Alt+Down, F4 and Space open it, and Enter goes to
 * Start; Escape closes an open list first.
 */
function Choice(props: {
  id: string;
  label: string;
  keys: string[];
  selected: string | undefined;
  placeholder?: string;
  disabled: boolean;
  open: boolean;
  onOpen(open: boolean): void;
  onPick(key: string): void;
  option(key: string): JSX.Element;
  tip?(key: string): string | undefined;
  class?: string;
  initial?: boolean;
}) {
  let root!: HTMLDivElement;
  let button!: HTMLButtonElement;
  const [active, setActive] = createSignal<string>();
  const step = (from: string | undefined, by: number) => {
    const keys = props.keys;
    if (!keys.length) return undefined;
    const at = from === undefined ? -1 : keys.indexOf(from);
    if (at < 0) return by > 0 ? keys[0] : keys[keys.length - 1];
    return keys[Math.max(0, Math.min(keys.length - 1, at + by))];
  };
  const show = () => { setActive(props.selected); props.onOpen(true); };
  const pick = (key: string | undefined) => {
    props.onOpen(false);
    button.focus();
    if (key !== undefined && key !== props.selected) props.onPick(key);
  };
  const onKey = (e: KeyboardEvent) => {
    if (props.disabled) return;
    const action = dialogKey("choice", e, props.open);
    const keys = props.keys;
    const to = (from: string | undefined) =>
      action === "first" ? keys[0] : action === "last" ? keys[keys.length - 1] : step(from, action === "next" ? 1 : -1);
    switch (action) {
      // Escape ("native") reaches the dialog's cancel, which closes the open list first.
      case "native": return;
      case "close": props.onOpen(false); return;
      case "start": e.preventDefault(); button.form?.requestSubmit(); return;
      case "ignore": e.preventDefault(); return;
      case "open": e.preventDefault(); show(); return;
      case "pick": e.preventDefault(); pick(active()); return;
      default: {
        e.preventDefault();
        if (props.open) { setActive(to(active())); return; }
        const next = to(props.selected);
        if (next !== undefined && next !== props.selected) props.onPick(next);
      }
    }
  };
  createEffect(() => {
    if (!props.open) return;
    const away = (e: PointerEvent) => { if (!root.contains(e.target as Node)) props.onOpen(false); };
    document.addEventListener("pointerdown", away, true);
    onCleanup(() => document.removeEventListener("pointerdown", away, true));
  });
  createEffect(() => {
    const key = active();
    if (!props.open || key === undefined) return;
    root.querySelector(`[data-key="${CSS.escape(key)}"]`)?.scrollIntoView({ block: "nearest" });
  });
  const optionId = (key: string) => `${props.id}-o-${props.keys.indexOf(key)}`;
  return (
    <div class={`ns-choice ${props.class ?? ""}`} ref={root}>
      <button type="button" ref={button} class="field ns-choice-btn" role="combobox" aria-label={props.label} aria-haspopup="listbox"
        aria-expanded={props.open} aria-controls={`${props.id}-list`} disabled={props.disabled} data-initial={props.initial || undefined}
        aria-activedescendant={props.open && active() !== undefined ? optionId(active()!) : undefined}
        title={props.selected !== undefined ? props.tip?.(props.selected) : undefined}
        onClick={() => (props.open ? props.onOpen(false) : show())} onKeyDown={onKey}>
        <span class="ns-choice-value">
          <Show when={props.selected !== undefined} fallback={<span class="ns-placeholder">{props.placeholder}</span>}>{props.option(props.selected!)}</Show>
        </span>
        <Icon name="chevron-down" size={14} class="ns-choice-chevron" />
      </button>
      <Show when={props.open}>
        <ul class="ns-choice-list" id={`${props.id}-list`} role="listbox" aria-label={props.label}>
          <For each={props.keys}>{(key, index) => (
            <li id={`${props.id}-o-${index()}`} data-key={key} role="option" aria-selected={key === props.selected}
              classList={{ on: key === props.selected, active: key === active() }} title={props.tip?.(key)}
              onPointerEnter={() => setActive(key)} onMouseDown={(e) => e.preventDefault()} onClick={() => pick(key)}>
              {props.option(key)}
            </li>
          )}</For>
        </ul>
      </Show>
    </div>
  );
}

/** One account row (AccountChoice::Delegate): the provider's logo, the bold name, then compact meters or the summary. */
function AccountLine(props: { row: () => AccountRow; selected: boolean }) {
  const minute = createMemo(() => Math.floor(now() / 60) * 60);
  const windows = createMemo(() => launchWindows(props.row().usage, minute()));
  const provider = () => props.row().provider;
  return (
    <>
      <span class="ns-acc-logo" style={{ color: AGENTS.has(provider()) ? `var(--z-agent-${agentKind(provider())})` : undefined }} aria-hidden="true">
        <Show when={!props.row().shell && AGENTS.has(provider())} fallback={<Icon name="terminal" size={26} />}>
          <AgentGlyph agent={agentKind(provider())} size={26} />
        </Show>
      </span>
      <span class="ns-acc-text">
        <b class="ell">{props.row().name}</b>
        <Show when={windows().length} fallback={
          <span class="ns-usage ell" classList={{ [`tone-${props.row().tone}`]: !!props.row().tone, selected: props.selected }}>{props.row().summary}</span>
        }>
          <span class="ns-meters"><UsageMeters windows={windows()} compact /></span>
        </Show>
      </span>
    </>
  );
}

function NewSessionForm(props: { s: () => NewSessionState; popup: () => string | undefined; setPopup(popup: string | undefined): void; confirm: ReturnType<typeof createConfirm> }) {
  const s = props.s;
  const form = () => s().form;
  // Until the core's first answer: nothing is locked, nothing can start.
  const fields = () => form()?.fields_enabled ?? true;
  const project = () => s().projects[s().project];
  const computer = () => s().computers[s().computer];
  const folder = () => s().folders[s().folder];
  const online = () => computer()?.online ?? false;
  const computerName = () => s().host || (machineCards[""]?.host ?? "");
  const opened = (name: string) => props.popup() === name;
  const toggle = (name: string) => (open: boolean) => props.setPopup(open ? name : opened(name) ? undefined : props.popup());

  // Keys keep the list rows while labels change in place.
  const projectKeys = createMemo(() => s().projects.map(projectKey), [], { equals: sameKeys });
  const computerKeys = createMemo(() => s().computers.map(computerKey), [], { equals: sameKeys });
  const folderKeys = createMemo(() => s().folders.map(folderKey), [], { equals: sameKeys });
  const accountKeys = createMemo(() => s().accounts.map(accountKey), [], { equals: sameKeys });
  // Each choice's selected key, read by the lists' key and click handlers too: a memo, so no
  // read outside the view's tracking makes a computation nobody disposes (it would read the
  // closed dialog's state and stop Escape from closing it).
  const projectSelected = createMemo(() => { const row = project(); return row ? projectKey(row) : undefined; });
  const computerSelected = createMemo(() => { const row = computer(); return row ? computerKey(row) : undefined; });
  const folderSelected = createMemo(() => { const row = folder(); return row ? folderKey(row) : undefined; });
  const projectOf = (key: string) => s().projects.find((row) => projectKey(row) === key);
  const computerOf = (key: string) => s().computers.find((row) => computerKey(row) === key);
  const folderOf = (key: string) => s().folders.find((row) => folderKey(row) === key);

  const projectOption = (key: string) => {
    // The project's colour as the Sessions list's header shows it.
    const color = () => {
      const row = projectOf(key);
      return row && !row.newProject ? projectColor(row) : undefined;
    };
    return (
      <>
        <Show when={!projectOf(key)?.newProject}><Icon name="projects" size={15} class="ns-project-icon" style={{ color: color() }} /></Show>
        <span class="ell">{projectOf(key)?.name}</span>
      </>
    );
  };
  const tile = (target: string): MachineState => {
    if (computerOf(target)?.online) return "online";
    return connectionOf(catalog.computers[target]?.status).tile === "connecting" ? "connecting" : "offline";
  };
  const computerOption = (key: string) => (
    <>
      <MachineTile os={osOf(machineCards[key]?.platform)} state={tile(key)} size={20} />
      <span class="ell">{computerOf(key)?.label}</span>
    </>
  );
  const folderOption = (key: string) => <span class="ell">{folderOf(key)?.name}</span>;

  // Browse… (581-588, 312-316): this computer's own dialog, else the folder browser over this one.
  const browse = () => {
    if (s().host === "") { void act.browseLocal(); return; }
    setNewSessionChild({ kind: "browse", target: s().host, computer: computerName(), project: project()?.id ?? "", path: folder()?.path ?? "" });
  };
  const browseRepository = () => {
    if (s().host === "") { void act.browseRepositoryLocal(); return; }
    setNewSessionChild({ kind: "browse-repository", target: s().host, computer: computerName(), parent: s().repositoryParent });
  };
  // A click on a disabled entry does nothing and keeps the menu open, as Qt's QMenu.
  const chooseExisting = () => {
    if (!form()?.choose_existing_enabled) return;
    props.setPopup(undefined);
    setNewSessionChild({ kind: "choose-worktree", target: s().host, computer: computerName(), project: project()?.id ?? "", path: folder()?.path ?? "", style: s().style });
  };
  const createManually = () => {
    if (!form()?.create_manually_enabled) return;
    props.setPopup(undefined);
    setNewSessionChild({ kind: "create-worktree", target: s().host, computer: computerName(), project: project()?.id ?? "", path: folder()?.path ?? "", catalog: s().worktrees.catalog, branch: s().name, style: s().style });
  };

  // The worktree options menu: Escape is the dialog's (it closes the menu first), arrows move.
  // As Qt's QMenu, opened by the keyboard its first enabled entry is active; by the mouse none is.
  let menu: HTMLDivElement | undefined;
  let menuByKeys = false;
  const toggleMenu = (e: MouseEvent) => {
    // A click the keyboard made (Space) has no pointer detail.
    menuByKeys = e.detail === 0;
    props.setPopup(opened("worktree") ? undefined : "worktree");
  };
  createEffect(() => {
    if (!opened("worktree")) return;
    const byKeys = menuByKeys;
    queueMicrotask(() => {
      const first = byKeys ? menu?.querySelector<HTMLElement>(".menu-item:not([aria-disabled=true])") : undefined;
      (first ?? menu)?.focus();
    });
    const away = (e: PointerEvent) => { if (!menu?.parentElement?.contains(e.target as Node)) props.setPopup(undefined); };
    document.addEventListener("pointerdown", away, true);
    onCleanup(() => document.removeEventListener("pointerdown", away, true));
  });
  const menuKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const items = [...(menu?.querySelectorAll<HTMLElement>(".menu-item:not([aria-disabled=true])") ?? [])];
    const at = items.indexOf(document.activeElement as HTMLElement);
    const down = e.key === "ArrowDown";
    // With no active entry, Down takes the first and Up the last.
    items[at < 0 ? (down ? 0 : items.length - 1) : (at + (down ? 1 : -1) + items.length) % items.length]?.focus();
  };

  // The account list: arrows choose; taking the focus never does (947-949).
  // Enter goes to the default button, as in Qt's dialog. Enabled unless the form is locked (698).
  let accountList!: HTMLUListElement;
  let formEl!: HTMLFormElement;
  const accountsEnabled = () => fields();
  const accountKeyDown = (e: KeyboardEvent) => {
    if (!accountsEnabled()) return;
    const count = s().accounts.length;
    if (!count) return;
    const at = s().account;
    let next: number;
    if (e.key === "ArrowDown") next = at < 0 ? 0 : Math.min(count - 1, at + 1);
    else if (e.key === "ArrowUp") next = at < 0 ? 0 : Math.max(0, at - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = count - 1;
    else if (e.key === "PageDown") next = at < 0 ? 0 : Math.min(count - 1, at + 4);
    else if (e.key === "PageUp") next = at < 0 ? 0 : Math.max(0, at - 4);
    else if (e.key === "Enter") { e.preventDefault(); if (!composing(e)) formEl.requestSubmit(); return; }
    else return;
    e.preventDefault();
    act.chooseAccount(next);
    queueMicrotask(() => accountList.querySelector(".on")?.scrollIntoView({ block: "nearest" }));
  };

  // The Git repository source focuses its URL field (606).
  let url: HTMLInputElement | undefined;
  createEffect(on(() => s().focusUrl, () => queueMicrotask(() => url?.focus()), { defer: true }));

  const start = (e: Event) => {
    e.preventDefault();
    if (!form()?.start_enabled || s().starting || s().submitted) return;
    void startNewSession((text) => props.confirm.ask(text));
  };

  // Enter on a line edit, the check box, the Effort list and the buttons Qt
  // made `setAutoDefault(false)` goes to Start (`dialogKey`); their
  // `data-keys` names the control. Cancel and Open Machines keep Enter.
  const formKey = (e: KeyboardEvent) => {
    const control = (e.target as HTMLElement | null)?.dataset?.keys as KeyControl | undefined;
    if (!control || control === "choice") return;
    const action = dialogKey(control, e);
    if (action === "start") { e.preventDefault(); formEl.requestSubmit(); }
    else if (action === "ignore") e.preventDefault();
  };

  return (
    <form class="ns-form" ref={formEl} onSubmit={start} onKeyDown={formKey}>
      <div class="dlg-head">
        <h2 id="ns-title">{tr("New session")}</h2>
        <span class="grow" />
        <button type="button" class="icon-btn" aria-label={tr("Close")} disabled={!(form()?.cancel_enabled ?? true)} onClick={() => closeNewSession()}><Icon name="x" size={16} /></button>
      </div>
      <div class="dlg-body ns-body">
        <div class="form-row">
          <label for="ns-project">{tr("Target")}</label>
          <div class="ns-target">
            <Choice id="ns-project" label={tr("Project")} keys={projectKeys()} selected={projectSelected()} disabled={!fields()}
              open={opened("project")} onOpen={toggle("project")} option={projectOption} class="ns-project" initial
              onPick={(key) => act.chooseProject(s().projects.findIndex((row) => projectKey(row) === key))} />
            <button type="button" class="icon-btn ns-manage" data-keys="button" data-tip={tr("Manage this project's folders")} aria-label={tr("Manage this project's folders")} disabled={!fields()}
              onClick={() => act.manageFolders()}><Icon name="settings" size={18} /></button>
            <Choice id="ns-computer" label={tr("Computer")} keys={computerKeys()} selected={computerSelected()} disabled={!fields()}
              open={opened("computer")} onOpen={toggle("computer")} option={computerOption} class="ns-computer"
              onPick={(key) => act.chooseComputer(s().computers.findIndex((row) => computerKey(row) === key))} />
          </div>
        </div>
        <Show when={s().newProjectRow}>
          <div class="form-row">
            <label for="ns-new-project" classList={{ "ns-reserved": !s().newProjectVisible }}>{tr("New project")}</label>
            <Show when={s().newProjectVisible} fallback={<span class="ns-reserved-field" />}>
              <input id="ns-new-project" class="field" data-keys="text" maxLength={80} placeholder={tr("Project name")} value={s().newProjectName} disabled={!fields()}
                onInput={(e) => act.setNewProjectName(e.currentTarget.value)} />
            </Show>
          </div>
        </Show>
        <div class="form-row">
          <label>{tr("Source")}</label>
          <div class="ns-seg" role="group" aria-label={tr("Source")}>
            <button type="button" class="ns-segbtn" data-keys="button" classList={{ on: !s().repository }} aria-pressed={!s().repository} disabled={!fields()} onClick={() => act.setSource(false)}>
              <Icon name="folder" size={14} />{tr("Working folder")}
            </button>
            <button type="button" class="ns-segbtn" data-keys="button" classList={{ on: s().repository }} aria-pressed={s().repository} disabled={!fields()} onClick={() => act.setSource(true)}>
              <Icon name="git-branch" size={14} />{tr("Git repository")}
            </button>
          </div>
        </div>
        <Show when={!s().repository} fallback={
          <>
            <div class="form-row">
              <label for="ns-url">{tr("Repository")}</label>
              <input id="ns-url" ref={url} class="field mono" data-keys="text" maxLength={4096} placeholder="https://github.com/owner/repository.git"
                title={tr("An SSH or HTTPS repository URL, on GitHub or another Git server.")} value={s().url} disabled={!fields() || !online()}
                onInput={(e) => act.setUrl(e.currentTarget.value)} />
            </div>
            <div class="form-row top">
              <label for="ns-destination">{tr("Clone to")}</label>
              <div class="ns-src-detail">
                <div class="row">
                  <input id="ns-destination" class="field mono grow" data-keys="text" maxLength={8192} value={s().destination} disabled={!fields() || !online()}
                    title={tr("The complete new checkout path. Edit its folder name or choose a parent folder with Browse.")}
                    onInput={(e) => act.setDestination(e.currentTarget.value)} />
                  <button type="button" id="ns-browse-repository" class="btn" data-keys="button" disabled={!fields() || !online()} onClick={browseRepository}>{tr("Browse…")}</button>
                </div>
              </div>
            </div>
          </>
        }>
          <div class="form-row">
            <label for="ns-folder">{tr("Working folder")}</label>
            <div class="row">
              <Choice id="ns-folder" label={tr("Working folder")} keys={folderKeys()} selected={folderSelected()} placeholder={tr("Choose a folder…")}
                disabled={!fields()} open={opened("folder")} onOpen={toggle("folder")} option={folderOption} tip={(key) => folderOf(key)?.tooltip} class="grow"
                onPick={(key) => act.chooseFolder(s().folders.findIndex((row) => folderKey(row) === key))} />
              <button type="button" id="ns-browse" class="btn" data-keys="button" disabled={!(form()?.browse_enabled ?? false)} onClick={browse}>{tr("Browse…")}</button>
            </div>
          </div>
          <div class="form-row top">
            <span />
            <div class="ns-src-detail">
              <div class="mono ns-path ell" title={s().folderPath} style={{ visibility: s().folderPath ? "visible" : "hidden" }}>{s().folderPath || " "}</div>
              <div class="ns-preview" classList={{ warn: !!form()?.outside_project }} title={form()?.preview ? trText(form()!.preview!) : undefined}>{form()?.preview ? trText(form()!.preview!) : ""}</div>
            </div>
          </div>
        </Show>

        <div class="form-row top">
          <label>{tr("Worktree")}</label>
          <div class="col ns-wt">
            <div class="row">
              <div class="ns-seg" role="group" aria-label={tr("Worktree")}>
                <button type="button" class="ns-segbtn" data-keys="button" classList={{ on: !!form()?.automatic }} aria-pressed={!!form()?.automatic} disabled={!(form()?.auto_enabled ?? false)}
                  title={tr("Create a generated branch and worktree when you start the session.")} onClick={() => act.setAuto(true)}>
                  <Icon name="git-branch" size={14} />{tr("Auto")}
                </button>
                <button type="button" class="ns-segbtn" data-keys="button" classList={{ on: !form()?.automatic }} aria-pressed={!form()?.automatic} disabled={!(form()?.none_enabled ?? true)}
                  title={tr("Start directly in the working folder.")} onClick={() => act.setAuto(false)}>
                  <Icon name="folder" size={14} />{tr("None")}
                </button>
              </div>
              <div class="ns-menu-anchor">
                <button type="button" id="ns-worktree-options" class="icon-btn" data-keys="button" data-tip={tr("Worktree options")} aria-label={tr("Worktree options")} aria-haspopup="menu" aria-expanded={opened("worktree")}
                  disabled={!(form()?.worktree_options_enabled ?? false)} onClick={toggleMenu}>
                  <Icon name="more" size={16} />
                </button>
                <Show when={opened("worktree")}>
                  <div class="menu ns-menu" role="menu" tabindex="-1" ref={menu} onKeyDown={menuKey}>
                    <Show when={s().worktrees.chooseVisible}>
                      <button type="button" class="menu-item" role="menuitem" aria-disabled={!form()?.choose_existing_enabled} onClick={chooseExisting}>
                        <Icon name="git-branch" size={14} /><span>{tr("Choose existing…")}</span>
                      </button>
                    </Show>
                    <Show when={s().worktrees.createVisible}>
                      <button type="button" class="menu-item" role="menuitem" aria-disabled={!form()?.create_manually_enabled} onClick={createManually}>
                        <Icon name="add" size={14} /><span>{tr("Create manually…")}</span>
                      </button>
                    </Show>
                  </div>
                </Show>
              </div>
            </div>
            <div class="mono ns-wt-target" title={form()?.worktree_tooltip ? trText(form()!.worktree_tooltip!) : undefined}>{form()?.worktree_target ?? ""}</div>
          </div>
        </div>

        <div class="form-row top">
          <label>{tr("Account and agent")}</label>
          <div class="col">
            <ul class="ns-accounts" ref={accountList} role="listbox" aria-label={tr("Account and agent")} tabindex={accountsEnabled() ? 0 : -1}
              aria-disabled={!accountsEnabled()} aria-activedescendant={s().account >= 0 ? `ns-account-${s().account}` : undefined} onKeyDown={accountKeyDown}>
              <For each={accountKeys()}>{(key) => {
                const row = rowOf(() => s().accounts.find((r) => accountKey(r) === key));
                const index = () => s().accounts.findIndex((r) => accountKey(r) === key);
                const selected = () => index() >= 0 && index() === s().account;
                return (
                  <li id={`ns-account-${index()}`} role="option" aria-selected={selected()} classList={{ on: selected() }} title={row().tooltip || undefined}
                    aria-label={row().accessible} onClick={() => { if (accountsEnabled()) act.chooseAccount(index()); }}>
                    <AccountLine row={row} selected={selected()} />
                  </li>
                );
              }}</For>
            </ul>
            <div class="row ns-acc-status">
              <span class="grow ns-status">{s().accountStatus}</span>
              <button type="button" class="btn small" data-keys="button" aria-label={tr("Refresh accounts")} disabled={!s().refreshEnabled} onClick={() => act.refreshAccounts()}>
                <Icon name="refresh" size={12} />{tr("Refresh")}
              </button>
            </div>
          </div>
        </div>

        <Show when={s().modelVisible}>
          <div class="form-row">
            <label for="ns-model">{tr("Model")}</label>
            <button type="button" id="ns-model" class="field ns-model-btn" data-keys="button" aria-label={tr("Model")} aria-haspopup="dialog" aria-expanded={s().pickerOpen}
              title={tr("Models OpenCode offers on this computer. Native default uses OpenCode's configuration.")} disabled={!s().modelEnabled}
              onClick={() => (s().pickerOpen ? act.closeModelPicker() : act.openModelPicker())}>
              <span class="ell">{s().modelButton}</span><Icon name="chevron-down" size={14} class="ns-choice-chevron" />
            </button>
          </div>
        </Show>
        <Show when={s().service.visible}>
          <div class="form-row">
            <span />
            <div class="row ns-service">
              <span class="grow ns-status">{s().service.reason}</span>
              <Show when={s().service.offer}>
                <button type="button" class="btn small" data-keys="button" title={s().service.tooltip} disabled={!s().service.enabled} onClick={() => act.startOpenCode()}>{s().service.text}</button>
              </Show>
            </div>
          </div>
        </Show>
        <Show when={s().effortVisible}>
          <div class="form-row">
            <label for="ns-effort">{tr("Effort")}</label>
            <select id="ns-effort" class="field ns-effort" data-keys="select" aria-label={tr("Effort")} title={tr("Variants of the selected model. Default uses the model's own setting.")}
              disabled={!fields()} value={s().effort} onChange={(e) => act.chooseEffort(e.currentTarget.value)}>
              <For each={s().efforts}>{(effort) => <option value={effort.value} selected={effort.value === s().effort}>{effort.label}</option>}</For>
            </select>
          </div>
        </Show>

        <div class="form-row">
          <label for="ns-name">{tr("Session name")}</label>
          <input id="ns-name" class="field mono" data-keys="text" value={s().name} disabled={!fields()} aria-invalid={!!form()?.name_problem}
            aria-describedby={form()?.name_problem ? "ns-name-problem" : undefined} onInput={(e) => act.setName(e.currentTarget.value)} />
        </div>
        <Show when={form()?.name_problem}>
          <div class="form-row"><span /><span id="ns-name-problem" class="ns-status">{form()?.name_problem ? trText(form()!.name_problem!) : ""}</span></div>
        </Show>
        <div class="form-row">
          <span />
          <label class="check" title={form()?.open_terminal_tooltip ? trText(form()!.open_terminal_tooltip) : undefined}>
            <input type="checkbox" data-keys="check" checked={s().openTerminal} disabled={!(form()?.open_terminal_enabled ?? false)} onChange={(e) => act.setOpenTerminal(e.currentTarget.checked)} />
            {tr("Open terminal window")}
          </label>
        </div>
      </div>
      {/* The error line, on its own line between the form and the buttons (264-265). */}
      <div class="ns-error" role="status">{s().error}</div>
      <div class="dlg-foot">
        <span class="spacer" />
        <Show when={s().noMachine}>
          <button type="button" class="btn" onClick={() => act.openMachines()}>{tr("Open Machines")}</button>
        </Show>
        <button type="button" class="btn" disabled={!(form()?.cancel_enabled ?? true)} onClick={() => closeNewSession()}>{tr("Cancel")}</button>
        <button type="submit" class="btn primary ns-start" disabled={!form()?.start_enabled || s().starting || s().submitted}>{form() ? trText(form()!.start_text) : tr("Start session")}</button>
      </div>
    </form>
  );
}

/** The one New session dialog, opened by the store (`openNewSession`) and closed by Cancel, Escape or an admitted Start. */
export function NewSessionDialog() {
  let el!: HTMLDialogElement;
  const confirm = createConfirm();
  const [popup, setPopup] = createSignal<string>();
  const open = () => !!newSession();
  createEffect(on(open, (shown) => {
    if (!shown) {
      setPopup(undefined);
      setNewSessionChild(undefined);
      confirm.cancel();
      if (el.open) el.close();
      return;
    }
    if (el.open) return;
    el.showModal();
    queueMicrotask(() => {
      const first = el.querySelector<HTMLElement>("[data-initial]:not(:disabled)")
        ?? el.querySelector<HTMLElement>(".dlg-body :is(input, select, button, [tabindex='0']):not(:disabled)");
      first?.focus();
    });
  }));
  const cancelEnabled = () => newSession()?.form?.cancel_enabled ?? true;
  return (
    <>
      <dialog class="dlg ns-dlg" ref={el} aria-labelledby="ns-title"
        onCancel={(e) => {
          // Escape closes the model picker, then an open list or menu, back to its button; the dialog closes only through the store.
          e.preventDefault();
          if (newSession()?.pickerOpen) {
            act.closeModelPicker();
            document.getElementById("ns-model")?.focus();
            return;
          }
          if (popup()) {
            const owner = el.querySelector<HTMLElement>("[aria-expanded='true']");
            setPopup(undefined);
            owner?.focus();
            return;
          }
          if (cancelEnabled()) closeNewSession();
        }}
        onClose={() => { if (newSession()) closeNewSession(); }}>
        <Show when={newSession()}>{(state) => <NewSessionForm s={state} popup={popup} setPopup={setPopup} confirm={confirm} />}</Show>
      </dialog>
      <Show when={newSession()?.pickerOpen}>
        <ModelPopover view={() => newSession()?.picker ?? EMPTY_PICKER} act={PICKER} anchor={() => document.getElementById("ns-model")} />
      </Show>
      <NewSessionChildren />
      <LaunchWarning confirm={confirm} />
    </>
  );
}

const EMPTY_PICKER: NewSessionState["picker"] = {
  groups: [], notice: "", all: { checked: false, enabled: false, tooltip: "" }, selected: undefined, query: "", favoritesAvailable: false,
};

/** The model popover's clicks, sent to the open dialog's picker. */
const PICKER: ModelPickerActions = {
  click: (value, star) => act.pickerClick(value, star),
  select: (value) => act.pickerSelect(value),
  toggleAll: () => act.pickerToggleAll(),
  search: (text) => act.pickerSearch(text),
  useNative: () => act.useNativeModel(),
  close: () => act.closeModelPicker(),
};

/** A peer's folder listings and New folder, bound to it (`list_directories`, `create_directory`). */
const folderCalls = (target: string): FolderBrowserCalls => ({
  list: (path, hidden) => bridge.listDirectories(target, path, hidden),
  create: (path) => bridge.createDirectory(target, path),
});

/** The control a child dialog gives the focus back to. */
const OPENER: Record<NewSessionChild["kind"], string> = {
  browse: "ns-browse", "browse-repository": "ns-browse-repository", "choose-worktree": "ns-worktree-options", "create-worktree": "ns-worktree-options",
};

/**
 * The child dialog New session opened (`newSessionChild`), each in its own
 * modal layer over it. Its answer goes to the store with the computer and
 * project it opened with; the store applies it only while they are still
 * the dialog's (585, 1123, 1134). Opening and inspecting them only reads.
 */
function NewSessionChildren() {
  const done = (kind: NewSessionChild["kind"]) => {
    setNewSessionChild(undefined);
    queueMicrotask(() => document.getElementById(OPENER[kind])?.focus());
  };
  // The computer's listed sessions, for the worktree picker's counts (G1). The memo reads only
  // the rows' computer and paths, and an equal list keeps its identity, so the picker counts
  // again only when that computer's sessions or their paths change.
  const sessionsOn = (target: string) => createMemo((): PickerSession[] =>
    Object.values(catalog.rows).filter((row) => row.computer === target).map((row) => ({ gitRoot: row.gitRoot, cwdCanonical: row.cwdCanonical })),
  [], { equals: sameSessions });
  const onlineOf = (target: string) => newSession()?.computers.find((row) => row.target === target)?.online ?? false;
  return (
    <Switch>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "browse" ? c : undefined; })()} keyed>{(child) => (
        <FolderBrowserDialog computer={child.computer} start={child.path} calls={folderCalls(child.target)}
          onChoose={(path) => act.applyBrowsedFolder(child.target, child.project, path)} onClose={() => done(child.kind)} />
      )}</Match>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "browse-repository" ? c : undefined; })()} keyed>{(child) => (
        <FolderBrowserDialog computer={child.computer} start={child.parent} calls={folderCalls(child.target)}
          onChoose={(path) => { if (newSession()?.host === child.target) act.setRepositoryParent(path); }} onClose={() => done(child.kind)} />
      )}</Match>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "choose-worktree" ? c : undefined; })()} keyed>{(child) => {
        const sessions = sessionsOn(child.target);
        return (
          <ChooseWorktreeDialog machine={child.computer} path={child.path} online={() => onlineOf(child.target)} style={() => child.style} sessions={sessions}
            reads={{ snapshot: () => bridge.worktreeSnapshot(child.target, child.path), catalog: (refresh) => bridge.worktreeCatalog(child.target, child.path, refresh) }}
            onUse={(path) => act.chooseExistingWorktree(child.target, child.project, path)} onClose={() => done(child.kind)} />
        );
      }}</Match>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "create-worktree" ? c : undefined; })()} keyed>{(child) => (
        <NewWorktreeDialog target={child.target} machine={child.computer} source={child.path} catalog={child.catalog} branch={child.branch} style={child.style}
          create={(branch, base, destination) => bridge.createWorktree(child.target, child.path, child.catalog, branch, base, destination)}
          pickLocal={(start) => bridge.pickLocalFolder(start)} browse={folderCalls(child.target)}
          onCreated={(created) => act.worktreeCreated(child.target, child.project, created.path, created.catalog)} onClose={() => done(child.kind)} />
      )}</Match>
    </Switch>
  );
}
