import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount, type JSX } from "solid-js";
import { tr } from "../../i18n";
import { localFolderProbe, teamAction } from "../../bridge";
import { createNewTeam, portOn, type NewTeamStore } from "../../store/teamsNew";
import { selected, setDetailTab, setPage } from "../../store/ui";
import { catalog as sessions, computerFresh } from "../../store/catalog";
import { accountsPage, loadAccounts } from "../../store/accounts";
import { groupOf, headerOf } from "../../store/organization";
import { showCatalog, showTeam, showTeamList, teams } from "../../store/teams";
import { accountChoices, catalogOf, computerChoices, coordinatorOf, draftLedBy, draftsOf, projectOfRow } from "../../store/newTeamSources";
import { NewTeam } from "./NewTeam";
import type { ComputerId } from "../../types";
import { TeamSettingsDialog } from "./TeamSettingsDialog";

/**
 * Task 200 (Qt PathProbe on its worker): the folder check of this computer's
 * typed folder, read shortly after the last keystroke; another computer's
 * folder or a browser without the host is taken as typed.
 */
function useFolderProbe(folder: () => string, remote: () => boolean = () => false) {
  const [probe, setProbe] = createSignal<{ folder: string; exists: boolean; git: boolean } | undefined>();
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(on(folder, (path) => {
    clearTimeout(timer);
    const typed = path.trim();
    if (!typed) { setProbe(undefined); return; }
    // WhiteWh fork: another computer's folder is taken as typed; its hgs checks it.
    if (remote()) { setProbe({ folder: typed, exists: true, git: true }); return; }
    timer = setTimeout(() => {
      localFolderProbe(typed).then(
        (answer) => { if (folder().trim() === typed) setProbe({ folder: typed, ...answer }); },
        () => { if (folder().trim() === typed) setProbe({ folder: typed, exists: true, git: true }); },
      );
    }, 250);
  }));
  onCleanup(() => clearTimeout(timer));
  const answered = () => { const now = probe(); return now && now.folder === folder().trim() ? now : undefined; };
  return {
    checking: () => !!folder().trim() && !answered(),
    exists: () => answered()?.exists ?? false,
    // Until the check answers, Git stays as Qt's form shows it: on.
    git: () => answered()?.git ?? true,
  };
}

/**
 * New team (Qt's NewTeamWindow, TeamNewTab): opening creates nothing; Start
 * planning creates the draft and its lead session. Task 56: it offers what New
 * team offers anywhere. Task 200: the selected local session's folder and
 * project (whose memory switch the memory modes follow), the folder check,
 * the draft the selected session leads shown again, and this computer's
 * drafts to reopen. `draft` opens one draft (the lead session's Team tab).
 */
export function NewTeamPage(props: { draft?: string; embedded?: boolean; computer?: ComputerId } = {}): JSX.Element {
  // WhiteWh fork (owner, 2026-11-18): a team started from a peer's session
  // lives on that computer: its hgs makes the draft, the lead and the members,
  // with that computer's catalog, accounts and folder.
  const computer = () => props.computer ?? "";
  const store = createNewTeam(portOn(computer, teamAction));
  const row = () => { const found = selected(); return found && found.computer === computer() && found.state !== "archived" ? found : undefined; };
  onMount(() => {
    store.openNew();
    void loadAccounts();
    if (props.draft) store.openDraft(props.draft);
    else if (row()?.cwdPath) store.setFolder(row()!.cwdPath);
  });
  createEffect(on(() => props.draft, (draft) => { if (draft) store.openDraft(draft); }, { defer: true }));
  // Qt F5: the selected session leads a draft: the form shows it again, once
  // the team list has it and while the owner has not started another form.
  const led = createMemo(() => draftLedBy(teams.teamOf(row()), row()));
  createEffect(on(led, (draft) => { if (draft && !store.draftId() && store.dirty().size === 0) store.openDraft(draft); }));
  // The catalog and this computer's team list are read while the form shows.
  onCleanup(showCatalog(computer()));
  onCleanup(showTeamList(computer()));
  const probe = useFolderProbe(() => store.fields().folder, () => computer() !== "");
  const computers = createMemo(() => computerChoices(sessions.computers, accountsPage().hosts ?? [], (computer) => computer === "" || computerFresh(computer)));
  const accounts = createMemo(() => accountChoices(accountsPage(), computer()));
  const catalog = createMemo(() => catalogOf(teams.catalog(computer())));
  const coordinator = createMemo(() => coordinatorOf(selected(), computer()));
  // This window's projects are its own; another computer's hgs does not know them.
  const project = createMemo(() => { const found = row(); return found && !computer() ? projectOfRow(groupOf(found.key), (id) => headerOf(id)?.name) : {}; });
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const draftTeam = () => (store.draftId() ? teams.team(computer(), store.draftId()) : undefined);
  createEffect(() => { const id = store.draftId(); if (settingsOpen() && id) onCleanup(showTeam(computer(), id)); });
  const form = (
    <NewTeam store={store} computer={computer()} computers={computers()} accounts={accounts()} accountsFor={(computer) => accountChoices(accountsPage(), computer)}
      catalog={catalog()} coordinator={coordinator()} git={probe.git()} folderOk={probe.exists()} folderChecking={probe.checking()}
      project={project().project} projectId={project().projectId}
      onOpenSettings={() => setSettingsOpen(true)} onAskDesigner={() => setPage("designer")}
      onCreated={() => { if (props.embedded) setDetailTab("team"); else setPage("sessions"); }}
      onDiscarded={() => { if (!props.embedded && row()?.cwdPath) store.setFolder(row()!.cwdPath); }} />
  );
  const settingsDialog = (
    <Show when={settingsOpen() && draftTeam()}>{(team) => <TeamSettingsDialog team={team()} computer={computer()} onClose={() => setSettingsOpen(false)} />}</Show>
  );
  if (props.embedded) return <div class="newteam-page embedded">{form}{settingsDialog}</div>;
  return (
    <div class="newteam-page">
      <header class="nt-page-head">
        <h1>{tr("New team")}</h1>
        <button type="button" class="btn ghost small" onClick={() => setPage("sessions")}>{tr("Back to sessions")}</button>
      </header>
      <DraftsList store={store} />
      {form}
      {settingsDialog}
    </div>
  );
}

/** Task 200: this computer's team drafts, each opened again in the form (Qt reopens a draft from its lead's session). */
function DraftsList(props: { store: NewTeamStore }) {
  const drafts = createMemo(() => draftsOf(teams.list("")));
  return (
    <Show when={drafts().length > 0}>
      <nav class="nt-drafts" aria-label={tr("Drafts")}>
        <span class="nt-hint">{tr("Drafts")}</span>
        <For each={drafts()}>{(draft) => (
          <button type="button" class="btn small ghost" classList={{ on: props.store.draftId() === draft.id }} data-draft={draft.id} title={draft.folder}
            onClick={() => props.store.openDraft(draft.id)}>{draft.name || tr("New team")}</button>
        )}</For>
        <Show when={props.store.draftId()}>
          <button type="button" class="btn small ghost" onClick={() => props.store.openNew()}>{tr("+ New")}</button>
        </Show>
      </nav>
    </Show>
  );
}
