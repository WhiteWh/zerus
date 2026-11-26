import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { tr } from "../../i18n";
import { Icon } from "../lib/icons";
import { memberRow, teams } from "../../store/teams";
import { setDetailTab, setPage, setSelectedKey } from "../../store/ui";
import { useModel, windowPort } from "./ProposalCards";
import {
  TeamActions, FINDING_LABELS, barActions, barChips, finishSummary, formText, moreItems, openQuestions, phaseModeChip, phaseModeState, phaseModeSwitch, questionClosed, teamModeBadge, stageName, subtreeImpact, taskButtons,
  leadMemberOf,
  type ActTask, type ActTeam, type BarAction, type BarChip, type TaskForm,
} from "./actions";
import type { ComputerId, TeamFindingItem, TeamPhaseView, TeamTaskView, TeamView } from "../../types";
import { noReportReply, remindMember } from "./noReport";
import { acceptedUnstarted, buttonText, menuText, phaseHeader, type HeaderPhase, type PhaseButton, type PhaseMenuId } from "./phaseHeader";
import { TextArea, TextInput } from "../lib/TextField";
import { teamTaskRead } from "../../bridge";

// Task 40: the team bar's actions and chips, and the task card's actions
// (ui/teams/actions.ts holds what they are and what they send).

/** A small modal with a text field: Finish's summary and Send back's comment. */
export function TextDialog(props: {
  title: string; hint: string; placeholder: string; action: string; danger?: boolean; count?: (text: string) => { count: string; ok: boolean };
  /** Task 199: the text it opens with (Iron rules edits the member's lines). */
  initial?: string;
  close: () => void; submit: (text: string) => void;
}) {
  let dialog!: HTMLDialogElement;
  let field!: HTMLTextAreaElement;
  const [text, setText] = createSignal(props.initial ?? "");
  const state = () => props.count?.(text()) ?? { count: "", ok: text().trim() !== "" };
  onMount(() => { dialog.showModal(); queueMicrotask(() => field.focus()); });
  onCleanup(() => { if (dialog.open) dialog.close(); });
  return (
    <dialog ref={dialog} class="dlg team-text-dialog" aria-labelledby="team-text-title" onCancel={(event) => { event.preventDefault(); props.close(); }}>
      <div class="dlg-head"><h2 id="team-text-title">{props.title}</h2></div>
      <div class="dlg-body">
        <p class="team-confirm-text">{props.hint}</p>
        <TextArea ref={field} class="field team-text-field" rows={5} placeholder={props.placeholder} value={text()} onInput={(event) => setText(event.currentTarget.value)} />
        <Show when={state().count}><div class="dim team-text-count">{state().count}</div></Show>
      </div>
      <div class="dlg-foot"><span class="spacer" />
        <button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button>
        <button type="button" class="btn" classList={{ primary: !props.danger, "solid-danger": !!props.danger }} disabled={!state().ok}
          onClick={() => { if (state().ok) { props.submit(text()); props.close(); } }}>{props.action}</button>
      </div>
    </dialog>
  );
}

/** D2: the owner's Findings page, with every finding's answer. */
export function FindingsDialog(props: { computer: ComputerId; team: ActTeam; close: () => void }) {
  let dialog!: HTMLDialogElement;
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  const [replan, setReplan] = createSignal<TeamFindingItem | undefined>();
  onMount(() => { dialog.showModal(); });
  onCleanup(() => { if (dialog.open) dialog.close(); });
  const answer = (item: TeamFindingItem, action: keyof typeof FINDING_LABELS) => {
    if (action === "replan") return setReplan(item);
    void model.finding(item.id, action, undefined, item.task ? Number(item.task) : undefined);
  };
  const items = () => props.team.findings?.items ?? [];
  return (
    <>
      <dialog ref={dialog} class="dlg team-findings-dialog" aria-labelledby="team-findings-title" onCancel={(event) => { event.preventDefault(); props.close(); }}>
        <div class="dlg-head"><h2 id="team-findings-title">{tr("Findings: %1", props.team.name)}</h2></div>
        <div class="dlg-body team-findings-list">
          <Show when={items().length > 0} fallback={<p class="dim">{tr("No findings on this page.")}</p>}>
            <For each={items()}>{(finding) => (
              <div class="team-finding-row" data-finding={finding.id}>
                <b class={`team-finding-${finding.severity}`}>{finding.severity.toUpperCase()}: {finding.title}</b>
                <Show when={finding.scenario}><p>{finding.scenario}</p></Show>
                <Show when={finding.file}><span class="dim mono">{finding.file}{finding.line > 0 ? `:${finding.line}` : ""}</span></Show>
                <Show when={finding.state}><span class="dim">{tr("state: %1", finding.state)}{finding.phase > 0 ? tr(", phase %1", finding.phase) : ""}{finding.note ? tr(" — %1", finding.note) : ""}</span></Show>
                <Show when={finding.proposal}><p class="dim">{tr("Proposal: %1", finding.proposal)}</p></Show>
                <div class="team-finding-actions">
                  <For each={Object.entries(FINDING_LABELS) as [keyof typeof FINDING_LABELS, string][]}>{(entry) => (
                    <button type="button" class="btn ghost small" data-finding-action={entry[0]} disabled={read().busy}
                      onClick={() => answer(finding, entry[0])}>{entry[1]}</button>
                  )}</For>
                </div>
              </div>
            )}</For>
          </Show>
          <Show when={read().notice}><p class="danger" role="status">{read().notice}</p></Show>
        </div>
        <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn" onClick={props.close}>{tr("Close")}</button></div>
      </dialog>
      <Show when={replan()}>{(finding) => (
        <TextDialog title={tr("Replan")} hint={tr("What should the lead change in the plan for \"%1\"? The team pauses until you resume it.", finding().title)}
          placeholder={tr("What should the lead change?")} action={tr("Replan")} close={() => setReplan(undefined)}
          submit={(proposal) => void model.finding(finding().id, "replan", proposal, finding().task ? Number(finding().task) : undefined)} />
      )}</Show>
    </>
  );
}

/** F7: a Done team's next goal, with the same people. */
export function NextGoalDialog(props: { computer: ComputerId; team: ActTeam; close: () => void }) {
  let dialog!: HTMLDialogElement;
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  const [goal, setGoal] = createSignal("");
  const [name, setName] = createSignal(props.team.name);
  onMount(() => { dialog.showModal(); });
  onCleanup(() => { if (dialog.open) dialog.close(); });
  const submit = async () => {
    if (!goal().trim()) return;
    const outcome = await model.nextGoal(goal(), name());
    if (!outcome || outcome === "cancelled" || outcome.outcome !== "done") return;
    // Task #296: Qt opens the new team; select its lead session on the Team
    // tab once the team list has it (the next-goal answer names its id).
    const created = (outcome.receipt as { team?: string } | null)?.team;
    if (created) {
      for (let wait = 0; wait < 20; ++wait) {
        const row = memberRow(props.computer, leadMemberOf(teams.list(props.computer), created));
        if (row) { setPage("sessions"); setSelectedKey(row.key); setDetailTab("team"); break; }
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
    }
    props.close();
  };
  const running = () => props.team.members.filter((member) => member.session).length;
  return (
    <dialog ref={dialog} class="dlg team-next-goal-dialog" aria-labelledby="team-next-goal-title" onCancel={(event) => { event.preventDefault(); props.close(); }}>
      <div class="dlg-head"><h2 id="team-next-goal-title">{tr("Next goal: %1", props.team.name)}</h2></div>
      <div class="dlg-body">
        <p class="team-confirm-text">{tr("A new team takes the next goal with this folder, its settings, the lead and the members. %1 of %2 sessions are still running and keep their context; the others start with their first task. This team stays Done.", running(), props.team.members.length)}</p>
        <label class="nt-field">{tr("Goal")}<TextArea class="field" rows={5} placeholder={tr("What the team delivers next, and what done means")} value={goal()} onInput={(event) => setGoal(event.currentTarget.value)} /></label>
        <label class="nt-field">{tr("Name")}<TextInput class="field" maxlength={80} value={name()} onInput={(event) => setName(event.currentTarget.value)} /></label>
        <Show when={read().notice}><p class="danger" role="status">{read().notice}</p></Show>
      </div>
      <div class="dlg-foot"><span class="spacer" />
        <button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button>
        <button type="button" class="btn primary" disabled={!goal().trim() || read().busy} onClick={() => void submit()}>{tr("Start next goal")}</button>
      </div>
    </dialog>
  );
}
export function PhaseModeControl(props: { computer: ComputerId; team: ActTeam; phase: number; owner?: boolean }) {
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  const state = createMemo(() => phaseModeState(props.team, props.phase, props.owner ?? true));
  const choose = (mode: "free" | "standard") => {
    const shown = state();
    if (!shown || !shown.canSwitch || shown.mode === mode) return;
    void model.phaseMode(props.phase, mode);
  };
  return (
    <Show when={state()}>{(shown) => (
      <span class="team-phase-mode" data-phase-mode={shown().mode} data-proposed={shown().proposed ?? ""}
        onClick={(event) => event.stopPropagation()}>
        <span class="team-phase-toggle" role="group" aria-label={tr("Phase mode")} title={shown().why || shown().tip}>
          <button type="button" data-mode="standard" aria-pressed={shown().mode === "standard"}
            classList={{ on: shown().mode === "standard" }}
            disabled={!shown().canSwitch || shown().mode === "standard" || read().busy}
            title={shown().mode === "standard" ? shown().tip : shown().why || shown().tip}
            onClick={(event) => { event.stopPropagation(); choose("standard"); }}>{tr("Standard")}</button>
          <button type="button" data-mode="free" aria-pressed={shown().mode === "free"}
            classList={{ on: shown().mode === "free" }}
            disabled={!shown().canSwitch || shown().mode === "free" || read().busy}
            title={shown().mode === "free" ? shown().tip : shown().why || shown().tip}
            onClick={(event) => { event.stopPropagation(); choose("free"); }}>{tr("Free")}</button>
        </span>
        <Show when={shown().proposed}>{(proposed) => (
          <span class="team-phase-proposed" title={tr("The lead's open proposal: %1", proposed() === "free" ? tr("Free") : tr("Standard"))}>{tr("proposed")}</span>
        )}</Show>
        <Show when={read().notice}><span class="danger team-phase-mode-notice" role="status">{read().notice}</span></Show>
      </span>
    )}</Show>
  );
}

/**
 * Task 196: a phase header's actions by its state (Qt TeamWorkspace phase
 * header): Reject and Accept a draft, Undo a send-back, Send back and Accept
 * a phase that waits for the owner, Add task to a running one, and the phase
 * menu (Edit, Move earlier/later, Pause or Remove pause, Requires owner
 * acceptance, Return to draft, Discard). The mode control stays beside them.
 */
export function PhaseActions(props: { computer: ComputerId; team: ActTeam & { phases: TeamPhaseView[] }; phase: number; owner?: boolean }) {
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  const record = createMemo((): HeaderPhase | undefined => props.team.phases.find((found) => found.n === props.phase));
  const header = createMemo(() => (record() ? phaseHeader(props.team, record()!) : undefined));
  // The header row clips and moves (virtualized): the menu opens in a portal
  // under its button, and closes on Escape or a click elsewhere.
  const [menu, setMenu] = createSignal<{ left: number; top: number } | undefined>();
  const toggle = (button: HTMLElement) => {
    if (menu()) return setMenu(undefined);
    const box = button.getBoundingClientRect();
    setMenu({ left: Math.max(8, Math.min(box.left, window.innerWidth - 280)), top: box.bottom + 4 });
  };
  createEffect(() => {
    if (!menu()) return;
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent && event.key !== "Escape") return;
      if (event.target instanceof Element && event.target.closest(".team-phase-menu, [data-action='phase-more']")) return;
      setMenu(undefined);
    };
    document.addEventListener("pointerdown", close, true);
    document.addEventListener("keydown", close, true);
    onCleanup(() => { document.removeEventListener("pointerdown", close, true); document.removeEventListener("keydown", close, true); });
  });
  const [dialog, setDialog] = createSignal<"reject" | "send_back" | "pause" | "edit" | "add_task" | undefined>();
  const press = (id: PhaseButton) => {
    switch (id) {
      case "accept": return void model.approvePhase(props.phase);
      case "undo": return void model.undoRejectPhase(props.phase);
      case "accept_phase": return void model.acceptOwnerPhase(props.phase);
      case "reject": case "send_back": case "add_task": return setDialog(id);
    }
  };
  const choose = (id: PhaseMenuId) => {
    setMenu(undefined);
    switch (id) {
      case "edit": return setDialog("edit");
      case "pause": return setDialog("pause");
      case "unpause": return void model.unpausePhase(props.phase);
      case "move_earlier": return void model.movePhase(props.phase, false);
      case "move_later": return void model.movePhase(props.phase, true);
      case "barrier": return void model.phaseBarrier(props.phase, !record()?.barrier);
      case "owner_acceptance": return void model.phaseAcceptance(props.phase, !record()?.owner_accepts);
      case "return_to_draft": return void model.returnToDraft(props.phase);
      case "discard": return void model.discardPhase(props.phase);
    }
  };
  return (
    <span class="team-phase-actions" data-phase={props.phase} onClick={(event) => event.stopPropagation()} onKeyDown={(event) => event.stopPropagation()}>
      <PhaseModeControl computer={props.computer} team={props.team} phase={props.phase} owner={props.owner} />
      <For each={header()?.buttons ?? []}>{(id) => (
        <button type="button" class="btn small" classList={{ primary: id === "accept" || id === "accept_phase", ghost: id === "undo" || id === "send_back" || id === "add_task" }}
          data-action={`phase-${id}`} disabled={read().busy} title={buttonText(id).tip} onClick={() => press(id)}>{buttonText(id).text}</button>
      )}</For>
      <Show when={(header()?.menu.length ?? 0) > 0}>
        <span class="team-phase-more">
          <button type="button" class="icon-btn team-phase-more-btn" aria-label={tr("Phase actions")} title={tr("Phase actions")} aria-haspopup="menu" aria-expanded={!!menu()}
            data-action="phase-more" disabled={read().busy} onClick={(event) => toggle(event.currentTarget)}><Icon name="ellipsis" size={16} /></button>
          <Show when={menu()}>{(at) => (<Portal>
            <div class="menu team-phase-menu" role="menu" aria-label={tr("Phase actions")} data-phase={props.phase}
              style={{ left: `${at().left}px`, top: `${at().top}px` }}>
              <For each={header()?.menu ?? []}>{(entry) => (
                <button type="button" role={entry.checked === undefined ? "menuitem" : "menuitemcheckbox"} aria-checked={entry.checked} class="menu-item"
                  data-item={entry.id} disabled={!entry.enabled} aria-disabled={!entry.enabled} onClick={() => choose(entry.id)}>
                  <Show when={entry.checked !== undefined}><span class="team-phase-check">{entry.checked ? <Icon name="check" size={12} /> : null}</span></Show>{menuText(entry.id)}
                </button>
              )}</For>
            </div>
          </Portal>)}</Show>
        </span>
      </Show>
      <Show when={read().notice}><span class="danger team-phase-notice" role="status">{read().notice}</span></Show>
      <Show when={dialog() === "reject"}>
        <TextDialog title={tr("Reject phase %1", props.phase)} hint={tr("Send the draft back to the lead with what should change")} placeholder={tr("What should change")}
          action={tr("Send back")} close={() => setDialog(undefined)} submit={(text) => void model.rejectPhase(props.phase, text)} />
      </Show>
      <Show when={dialog() === "send_back"}>
        <TextDialog title={tr("Send phase %1 back", props.phase)} hint={tr("The lead reworks the phase with your comment")} placeholder={tr("What should change")}
          action={tr("Send back")} close={() => setDialog(undefined)} submit={(text) => void model.sendBackPhase(props.phase, text)} />
      </Show>
      <Show when={dialog() === "pause"}>
        <TextDialog title={tr("Pause phase %1", props.phase)} hint={tr("Phase %1 stays accepted but does not start until the pause is removed.", props.phase)}
          placeholder={tr("Why it waits, for the lead")} action={tr("Pause phase")} count={() => ({ count: "", ok: true })}
          close={() => setDialog(undefined)} submit={(text) => void model.pausePhase(props.phase, text)} />
      </Show>
      <Show when={dialog() === "edit"}>
        <EditPhaseDialog team={props.team} phase={props.phase} close={() => setDialog(undefined)}
          submit={(title, updates) => void model.editPhase(props.phase, title, updates)} />
      </Show>
      <Show when={dialog() === "add_task"}>
        <AddTaskDialog team={props.team} phase={props.phase} close={() => setDialog(undefined)}
          submit={(task) => void model.addTasks(props.phase, [task])} />
      </Show>
    </span>
  );
}

/** Task 196: Edit phase (Qt editPhaseDialog): its title and the titles of its tasks that have not started; Save sends only what changed. */
function EditPhaseDialog(props: {
  team: ActTeam & { phases: TeamPhaseView[] }; phase: number; close: () => void;
  submit: (title: string | undefined, updates: { id: number; title: string }[]) => void;
}) {
  let dialog!: HTMLDialogElement;
  const before = props.team.phases.find((found) => found.n === props.phase)?.title ?? "";
  const tasks = props.team.tasks.filter((task) => task.phase === props.phase);
  const [title, setTitle] = createSignal(before);
  const [titles, setTitles] = createSignal<Record<number, string>>(Object.fromEntries(tasks.map((task) => [task.id, task.title])));
  const editable = (task: ActTask) => ["pending", "draft"].includes(task.status) && task.attempts === 0;
  onMount(() => dialog.showModal());
  onCleanup(() => { if (dialog.open) dialog.close(); });
  const save = () => {
    const next = title().trim();
    const updates = tasks.filter(editable).map((task) => ({ id: task.id, title: (titles()[task.id] ?? "").trim() }))
      .filter((update) => update.title && update.title !== tasks.find((task) => task.id === update.id)?.title);
    props.submit(next && next !== before ? next : undefined, updates);
    props.close();
  };
  return (
    <dialog ref={dialog} class="dlg team-edit-phase" aria-labelledby="team-edit-phase-title" onCancel={(event) => { event.preventDefault(); props.close(); }}>
      <div class="dlg-head"><h2 id="team-edit-phase-title">{tr("Edit phase %1", props.phase)}</h2></div>
      <div class="dlg-body">
        <p class="team-confirm-text">{tr("Phase %1 stays accepted and keeps its place. Only tasks whose work has not started change here.", props.phase)}</p>
        <label class="dim">{tr("Phase title")}</label>
        <TextInput class="field" maxLength={120} value={title()} onInput={(event) => setTitle(event.currentTarget.value)} data-field="phase-title" />
        <label class="dim">{tr("Tasks")}</label>
        <For each={tasks}>{(task) => (
          <div class="team-edit-phase-task"><span class="dim">#{task.id}</span>
            <TextInput class="field" maxLength={200} value={titles()[task.id] ?? ""} disabled={!editable(task)} data-task={task.id}
              title={editable(task) ? undefined : tr("Only a pending task changes here")}
              onInput={(event) => setTitles({ ...titles(), [task.id]: event.currentTarget.value })} />
          </div>
        )}</For>
      </div>
      <div class="dlg-foot"><span class="spacer" />
        <button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button>
        <button type="button" class="btn primary" onClick={save}>{tr("Save")}</button>
      </div>
    </dialog>
  );
}

/** Task 196: Add task... to a running phase (Qt addLateDialog): its title, member and description; hgs warns before it joins. */
function AddTaskDialog(props: { team: ActTeam; phase: number; close: () => void; submit: (task: { title: string; assignee?: string; description?: string }) => void }) {
  let dialog!: HTMLDialogElement;
  const members = props.team.members.filter((member) => !member.lead);
  const [title, setTitle] = createSignal("");
  const [assignee, setAssignee] = createSignal(members[0]?.id ?? "");
  const [description, setDescription] = createSignal("");
  onMount(() => dialog.showModal());
  onCleanup(() => { if (dialog.open) dialog.close(); });
  const add = () => {
    if (!title().trim()) return;
    props.submit({ title: title().trim(), assignee: assignee() || undefined, description: description().trim() || undefined });
    props.close();
  };
  return (
    <dialog ref={dialog} class="dlg team-add-task" aria-labelledby="team-add-task-title" onCancel={(event) => { event.preventDefault(); props.close(); }}>
      <div class="dlg-head"><h2 id="team-add-task-title">{tr("Add a task to phase %1", props.phase)}</h2></div>
      <div class="dlg-body">
        <p class="team-confirm-text">{tr("Add a task to this running phase: Zerus warns first, then you confirm")}</p>
        <label class="dim">{tr("Title")}</label>
        <TextInput class="field" maxLength={200} value={title()} onInput={(event) => setTitle(event.currentTarget.value)} data-field="task-title" />
        <label class="dim">{tr("Member")}</label>
        <select class="field" value={assignee()} onChange={(event) => setAssignee(event.currentTarget.value)} data-field="task-assignee">
          <For each={members}>{(member) => <option value={member.id}>{member.role || member.id}</option>}</For>
        </select>
        <label class="dim">{tr("Description")}</label>
        <TextArea class="field" rows={4} value={description()} onInput={(event) => setDescription(event.currentTarget.value)} />
      </div>
      <div class="dlg-foot"><span class="spacer" />
        <button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button>
        <button type="button" class="btn primary" disabled={!title().trim()} onClick={add}>{tr("Add task")}</button>
      </div>
    </dialog>
  );
}

/** Task 196: a task as the plan's editor reads it (core TaskView: description and criteria of a task not yet handed out). */
type EditTask = ActTask & { description?: string; criteria?: string[] };
type EditTeam = ActTeam & { phases: TeamPhaseView[]; tasks: EditTask[] };

/**
 * Task 196: editing the plan (Qt planning mode, TeamWorkspace.cpp 3304-3316,
 * 1968): while the team plans, "+ Task", a card's click edits it, a drag
 * between cards links them and an edge's click removes it, each sent as the
 * whole plan at the revision the owner saw (core plan_edit.rs). Outside
 * planning, the tasks of a draft or of an accepted phase that has not started
 * link and unlink through edit-draft (Qt linkDraft).
 */
export function createPlanEditing(
  where: () => { computer: ComputerId; team: EditTeam },
  readTask: (computer: ComputerId, team: string, task: number) => Promise<{ task: EditTask }> = teamTaskRead,
) {
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, where, changed));
  const read = () => (tick(), model);
  const [dialog, setDialog] = createSignal<{ task?: EditTask } | undefined>();
  const planning = () => where().team.status === "planning";
  const task = (id: number) => where().team.tasks.find((found) => found.id === id);
  const canLink = (id: number) => {
    const found = task(id);
    if (!found || found.status === "cancelled") return false;
    if (planning()) return true;
    return found.attempts === 0 && (found.status === "draft" || (found.status === "pending" && acceptedUnstarted(where().team, found.phase)));
  };
  const link = (dependency: number, id: number, add: boolean) => {
    if (planning()) return void model.planEdit({ edit: "link", dependency, task: id, add });
    const found = task(id);
    if (found) void model.linkDraft(found, dependency, add);
  };
  const editing = {
    canLink,
    link: (dependency: number, id: number) => link(dependency, id, true),
    unlink: (dependency: number, id: number) => {
      void windowPort.confirm({ name: "teamUnlinkConfirm", title: tr("Remove dependency?"), text: tr("#%1 no longer waits for #%2.", id, dependency), action: tr("Remove") })
        .then((yes) => { if (yes) link(dependency, id, false); });
    },
    editTask: (id: number) => { if (planning()) setDialog({ task: task(id) }); },
  };
  const view = () => (
    <Show when={dialog()}>{(open) => (
      <PlanTaskDialog team={where().team} task={open().task} close={() => setDialog(undefined)}
        load={open().task ? () => readTask(where().computer, where().team.id, open().task!.id).then((detail) => detail.task) : undefined}
        save={(fields) => void model.planEdit({ edit: "task", task: open().task ? { id: open().task!.id, ...fields } : fields })}
        remove={open().task ? () => {
          const id = open().task!.id;
          void windowPort.confirm({ name: "teamRemoveTaskConfirm", title: tr("Remove task?"), text: tr("Remove #%1 from the plan? The dependencies on it go too.", id), action: tr("Remove") })
            .then((yes) => { if (yes) void model.planEdit({ edit: "remove_task", task: id }); });
        } : undefined} />
    )}</Show>
  );
  return {
    planning,
    editing: () => editing,
    addTask: () => setDialog({}),
    dialog: view,
    notice: () => read().notice,
  };
}

/** Task 196: the plan's task dialog while the team plans (Qt editTask): title, member, phase, description and criteria. */
function PlanTaskDialog(props: {
  team: EditTeam; task?: EditTask; close: () => void;
  save: (fields: Record<string, unknown>) => void; remove?: () => void;
  /** Task #57: the task's whole record; the polled plan clips a long description. */
  load?: () => Promise<EditTask>;
}) {
  let dialog!: HTMLDialogElement;
  const members = props.team.members.filter((member) => !member.lead);
  const [title, setTitle] = createSignal(props.task?.title ?? "");
  const [assignee, setAssignee] = createSignal(props.task?.assignee ?? members[0]?.id ?? "");
  const [phase, setPhase] = createSignal(String(props.task?.phase ?? 1));
  const [description, setDescription] = createSignal(props.task?.description ?? "");
  const [criteria, setCriteria] = createSignal((props.task?.criteria ?? []).join("\n"));
  // Task #57: an existing task's description and criteria come from its own
  // read, never the polled plan's clipped copy, so a save never writes a clip
  // back. Until the read answers they cannot be edited or saved.
  const [reading, setReading] = createSignal(Boolean(props.load));
  const [unread, setUnread] = createSignal(false);
  onMount(() => {
    dialog.showModal();
    props.load?.().then((full) => {
      setDescription(full.description ?? "");
      setCriteria((full.criteria ?? []).join("\n"));
      setReading(false);
    }, () => { setUnread(true); setReading(false); });
  });
  onCleanup(() => { if (dialog.open) dialog.close(); });
  const save = () => {
    if (!title().trim() || reading() || unread()) return;
    const fields: Record<string, unknown> = {
      title: title().trim(),
      assignee: assignee(),
      phase: Math.max(1, Number(phase()) || 1),
      criteria: criteria().split("\n").map((line) => line.trim()).filter(Boolean),
    };
    // A description the view did not get (a long one) is never overwritten blank.
    if (description().trim() || props.task?.description) fields.description = description().trim();
    props.save(fields);
    props.close();
  };
  return (
    <dialog ref={dialog} class="dlg team-plan-task" aria-labelledby="team-plan-task-title" onCancel={(event) => { event.preventDefault(); props.close(); }}>
      <div class="dlg-head"><h2 id="team-plan-task-title">{props.task ? tr("Task %1", props.task.id) : tr("New task")}</h2></div>
      <div class="dlg-body">
        <label class="dim">{tr("Title")}</label>
        <TextInput class="field" maxLength={200} value={title()} onInput={(event) => setTitle(event.currentTarget.value)} data-field="task-title" />
        <label class="dim">{tr("Member")}</label>
        <select class="field" value={assignee()} onChange={(event) => setAssignee(event.currentTarget.value)} data-field="task-assignee">
          <For each={members}>{(member) => <option value={member.id}>{member.role || member.id}</option>}</For>
        </select>
        <label class="dim">{tr("Phase")}</label>
        <TextInput class="field" type="number" min={1} value={phase()} onInput={(event) => setPhase(event.currentTarget.value)} data-field="task-phase" />
        <label class="dim">{tr("Description")}</label>
        <TextArea class="field" rows={4} value={description()} disabled={reading() || unread()} onInput={(event) => setDescription(event.currentTarget.value)} data-field="task-description" />
        <label class="dim">{tr("Acceptance criteria, one per line")}</label>
        <TextArea class="field" rows={3} value={criteria()} disabled={reading() || unread()} onInput={(event) => setCriteria(event.currentTarget.value)} data-field="task-criteria" />
        <Show when={reading()}><p class="dim" data-field="task-reading">{tr("Reading the task…")}</p></Show>
        <Show when={unread()}><p class="dim" data-field="task-unread">{tr("The task could not be read; close it and open it again.")}</p></Show>
      </div>
      <div class="dlg-foot">
        <Show when={props.remove}><button type="button" class="btn danger" onClick={() => { props.remove!(); props.close(); }}>{tr("Remove task")}</button></Show>
        <span class="spacer" />
        <button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button>
        <button type="button" class="btn primary" disabled={!title().trim() || reading() || unread()} onClick={save}>{tr("Save")}</button>
      </div>
    </dialog>
  );
}

/** Task #59: the team bar's mode (Qt teamHeaderPlanMode) and, for the free phase, its lock back to standard. */
/**
 * Task #12 (owner, 2026-11-10): the planning team's Free planning switch. It
 * shows the team's stored mode (`plan_mode`, the default of every phase
 * without its own) and stores a switch in hgs at once, so each phase's toggle
 * follows it and both survive a reload; Approve starts in it. A choice on its
 * way shows until the next read has it; a refusal shows why and the switch
 * reads the stored mode again.
 */
export function PlanModeOption(props: { computer: ComputerId; team: ActTeam }) {
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  const [pending, setPending] = createSignal<"free" | "standard" | null>(null);
  const stored = (): "free" | "standard" => (props.team.plan_mode === "free" ? "free" : "standard");
  createEffect(() => { if (pending() && pending() === stored()) setPending(null); });
  const checked = () => (pending() ?? stored()) === "free";
  const choose = async (free: boolean) => {
    const mode = free ? "free" : "standard";
    if (mode === stored() && !pending()) return;
    setPending(mode);
    const outcome = await model.teamMode(mode);
    if (!outcome || outcome === "cancelled" || outcome.outcome !== "done") setPending(null);
  };
  return (
    <label class="team-proposal-option" data-plan-mode={checked() ? "free" : "standard"} title={tr("One open phase that takes tasks as they come until you close it")}>
      <input type="checkbox" checked={checked()} disabled={read().busy}
        onChange={(event) => { const free = event.currentTarget.checked; event.currentTarget.checked = checked(); void choose(free); }} />{tr("Free planning")}
      <Show when={read().notice}><span class="danger team-phase-mode-notice" role="status">{read().notice}</span></Show>
    </label>
  );
}

export function TeamModeControl(props: { computer: ComputerId; team: ActTeam }) {
  const badge = createMemo(() => teamModeBadge(props.team));
  const lock = createMemo(() => { const b = badge(); return b && b.phase > 0 ? phaseModeSwitch(props.team, b.phase) : undefined; });
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  return (
    <Show when={badge()}>{(shown) => (
      <span class="team-mode" data-mode={shown().mode}>
        <span class={`pt-chip pt-chip-${shown().mode}`} title={shown().tip}>{shown().text}</span>
        <Show when={lock()?.mode === "standard" ? lock() : undefined}>{(switching) => (
          <button type="button" class="btn ghost small" data-action="phase-mode" data-mode="standard" disabled={!switching().enabled || read().busy}
            title={tr("Switch phase %1 to standard planning: its tasks stay, new work is planned in a new phase", switching().phase)}
            onClick={() => void model.phaseMode(switching().phase, "standard")}><span class="ell">{switching().text}</span></button>
        )}</Show>
        <Show when={read().notice}><span class="danger team-phase-mode-notice" role="status">{read().notice}</span></Show>
      </span>
    )}</Show>
  );
}

/** The bar's chips: the pending phase (opens it), Finish waiting, waiting for the lead. */
export function TeamBarChips(props: { team: ActTeam; onPhase: (phase: number) => void; onFinish: () => void; onLead?: () => void }) {
  const chips = createMemo(() => barChips(props.team));
  const click = (chip: BarChip) => {
    if (chip.id === "pendingPhase" && chip.phase) props.onPhase(chip.phase);
    else if (chip.id === "finishWaits") props.onFinish();
    else if (chip.id === "leadStalled") props.onLead?.();
  };
  return (
    <For each={chips()}>{(chip) => chip.id === "waitingForLead" || chip.id === "teamPaused"
      ? <span class="team-chip" classList={{ accent: chip.id === "waitingForLead", warn: chip.id === "teamPaused" }} data-chip={chip.id} title={chip.tip}>{chip.text}</span>
      : <button type="button" class="team-chip link" classList={{ warn: chip.id === "leadStalled" }} data-chip={chip.id} title={chip.tip} onClick={() => click(chip)}>{chip.text}</button>}
    </For>
  );
}

const ICON: Partial<Record<BarAction["id"], string>> = { approve: "play", resume: "play", pause: "pause", finish: "check", sendBack: "rotate-ccw", stop: "circle-stop", discard: "trash", findings: "search", nextGoal: "arrow-right" };

/**
 * The bar's actions. `planMode` is the owner's free/standard choice for the
 * approval; `sendToCoordinator` puts Send back's message to the coordinator.
 */
export function TeamBarActions(props: { computer: ComputerId; team: ActTeam; planMode: string; sendToCoordinator: (text: string) => void; finishOpen?: () => boolean; setFinishOpen?: (open: boolean) => void; compact?: "none" | "low" | "high"; onSettings?: () => void; onCatalog?: () => void }) {
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  const [localFinish, setLocalFinish] = createSignal(false);
  const finishOpen = () => props.finishOpen?.() ?? localFinish();
  const setFinish = (open: boolean) => (props.setFinishOpen ?? setLocalFinish)(open);
  const [sendBack, setSendBack] = createSignal(false);
  const [findingsOpen, setFindingsOpen] = createSignal(false);
  const [nextGoalOpen, setNextGoalOpen] = createSignal(false);
  const actions = createMemo(() => barActions(props.team, props.planMode));
  // Task #427: below 1100 px the lower-priority actions (Resume after limits,
  // Team settings, Catalog) fold into More; below 900 px Pause and Stop fold
  // too, so the strip stays one row.
  const overflow = createMemo<ReadonlySet<string>>(() => {
    if (props.compact === "low") return new Set(["resumeLimits"]);
    // Owner, 2026-11-09: the primary action (Approve the plan) never folds.
    if (props.compact === "high") return new Set(actions().filter((action) => !action.primary).map((action) => action.id));
    return new Set();
  });
  const moved = () => props.compact === "low" || props.compact === "high";
  const [moreOpen, setMoreOpen] = createSignal(false);
  let moreButton: HTMLButtonElement | undefined, moreMenu: HTMLDivElement | undefined;
  // The team bar clips its overflow: the open menu goes to the top layer and
  // stands under its button, right-aligned, inside the window.
  createEffect(() => {
    if (!moreOpen() || !moreButton || !moreMenu) return;
    const menu = moreMenu, button = moreButton;
    if (typeof menu.showPopover === "function" && !menu.matches(":popover-open")) menu.showPopover();
    const place = () => {
      const box = button.getBoundingClientRect();
      const left = Math.max(8, Math.min(box.right - menu.offsetWidth, window.innerWidth - menu.offsetWidth - 8));
      const below = box.bottom + 4;
      const top = below + menu.offsetHeight > window.innerHeight - 8 ? Math.max(8, box.top - menu.offsetHeight - 4) : below;
      menu.style.left = `${left}px`;
      menu.style.top = `${top}px`;
    };
    place();
    const away = (event: Event) => {
      if (!button.contains(event.target as Node) && !menu.contains(event.target as Node)) setMoreOpen(false);
    };
    const key = (event: KeyboardEvent) => { if (event.key === "Escape") { event.stopPropagation(); setMoreOpen(false); button.focus(); } };
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", key, true);
    window.addEventListener("resize", place);
    onCleanup(() => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", key, true);
      window.removeEventListener("resize", place);
      if (typeof menu.hidePopover === "function" && menu.matches(":popover-open")) menu.hidePopover();
    });
  });
  const inline = createMemo(() => actions().filter((action) => !overflow().has(action.id)));
  const folded = () => actions().filter((action) => overflow().has(action.id));
  const moreIds = () => ["settings", "catalog", ...folded().map((action) => action.id)].join(" ");
  const impact = () => subtreeImpact(props.team, teams.list(props.computer)?.teams ?? []);
  const run = (action: BarAction) => {
    switch (action.id) {
      case "resumeLimits": return void model.resumeAfterLimits(!action.checked);
      case "finish": return setFinish(true);
      case "discard": return void model.discard();
      case "sendBack": return setSendBack(true);
      case "approve": return void model.approve(props.planMode);
      case "pause": return void model.pause(impact());
      case "resume": return void model.resume();
      case "stop": return void model.stop(impact());
      case "archive": return void model.archive();
      case "findings": return setFindingsOpen(true);
      case "nextGoal": return setNextGoalOpen(true);
    }
  };
  return (
    <div class="team-bar-actions" role="toolbar" aria-label={tr("Team actions")}>
      <For each={inline()}>{(action) => (
        <button type="button" class="btn small" data-action={action.id} title={action.tip}
          classList={{ primary: action.primary, danger: action.danger, ghost: action.id === "discard", on: !!action.checked }}
          aria-pressed={action.checked === undefined ? undefined : action.checked} disabled={!action.enabled || read().busy}
          onClick={() => run(action)}>
          <Show when={ICON[action.id]}><Icon name={ICON[action.id]!} size={12} /></Show>{action.text}
        </button>
      )}</For>
      <Show when={moved()}>
        <button ref={moreButton} type="button" class="btn small ghost team-more-btn" data-more="" data-overflow={moreIds()}
          aria-haspopup="menu" aria-expanded={moreOpen()} onClick={() => setMoreOpen(!moreOpen())}>
          {tr("More")}<Icon name="chevron-down" size={12} />
        </button>
        <div ref={moreMenu} popover="manual" class="menu team-more-menu" role="menu" aria-label={tr("More team actions")} hidden={!moreOpen()}>
          <Show when={props.onSettings}>
            <button type="button" role="menuitem" class="menu-item" data-more-item="settings"
              onClick={() => { setMoreOpen(false); props.onSettings?.(); }}>{tr("Team settings")}</button>
          </Show>
          <Show when={props.onCatalog}>
            <button type="button" role="menuitem" class="menu-item" data-more-item="catalog"
              onClick={() => { setMoreOpen(false); props.onCatalog?.(); }}>{tr("Catalog")}</button>
          </Show>
          <For each={folded()}>{(action) => (
            <button type="button" role="menuitem" class="menu-item" data-more-item={action.id} disabled={!action.enabled || read().busy}
              title={action.tip} onClick={() => { setMoreOpen(false); run(action); }}>
              <Show when={ICON[action.id]}><Icon name={ICON[action.id]!} size={12} /></Show>{action.text}
            </button>
          )}</For>
        </div>
      </Show>
      <Show when={read().notice}><span class="team-proposal-notice" role="status">{read().notice}</span></Show>
      <Show when={finishOpen()}>
        <TextDialog title={tr("Finish %1?", props.team.name)} hint={tr("Every task is decided. The summary goes to the lead and the team becomes Done.")}
          placeholder={tr("What the team delivered, and anything left undone…")} action={tr("Finish team")} count={finishSummary}
          close={() => setFinish(false)} submit={(text) => void model.finish(text)} />
      </Show>
      <Show when={sendBack()}>
        <TextDialog title={tr("Send back to the coordinator")} hint={tr("What should change in the plan?")} placeholder={tr("What should change in the plan?")}
          action={tr("Send back")} close={() => setSendBack(false)} submit={(text) => props.sendToCoordinator(text)} />
      </Show>
      <Show when={findingsOpen()}><FindingsDialog computer={props.computer} team={props.team} close={() => setFindingsOpen(false)} /></Show>
      <Show when={nextGoalOpen()}><NextGoalDialog computer={props.computer} team={props.team} close={() => setNextGoalOpen(false)} /></Show>
    </div>
  );
}

/** The task card's actions: the footer by status, More, the inline forms and Reassign's member list. */
export function TaskActions(props: { computer: ComputerId; team: ActTeam; task: ActTask }) {
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  const [form, setForm] = createSignal<TaskForm | undefined>();
  const [reason, setReason] = createSignal("");
  const [returnLater, setReturnLater] = createSignal(false);
  const [menu, setMenu] = createSignal<"more" | "reassign" | undefined>();
  const [delegate, setDelegate] = createSignal(false);
  // Task #415: the kept last reply of a No report attempt, and whether its
  // whole text is shown.
  const [replyOpen, setReplyOpen] = createSignal(false);
  const [remindNotice, setRemindNotice] = createSignal("");
  const reply = () => noReportReply(teams.inbox(props.computer, props.team.id)?.items ?? [], props.task.id);
  const remind = async () => {
    const outcome = await remindMember(props.computer, props.team as unknown as TeamView, props.task as unknown as TeamTaskView);
    setRemindNotice(outcome === "missing" ? tr("The member's session is not open; open it to send the reminder.")
      : outcome === "draft" ? tr("The session already has a draft; the reminder was not added.") : "");
  };
  const open = (next: TaskForm) => { setForm(next); setReason(""); setReturnLater(false); setMenu(undefined); };
  const words = () => (form() ? formText(props.team, props.task, form()!) : undefined);
  const peers = () => props.team.members.filter((member) => member.id !== props.task.assignee && !member.lead);
  const button = (id: string) => {
    switch (id) {
      case "accept": return void model.accept(props.task);
      case "release": return void model.release(props.task);
      case "reassign": return setMenu(menu() === "reassign" ? undefined : "reassign");
      case "takeOver": return void model.takeOver(props.task);
      case "cancel": return void model.cancel(props.task);
    }
  };
  const item = (id: string, phase?: number, next?: TaskForm) => {
    setMenu(undefined);
    if (next) return open(next);
    switch (id) {
      case "retry": return void model.retry(props.task);
      case "release": return void model.release(props.task);
      case "move": return void model.move(props.task, phase!);
      case "reassign": return setMenu("reassign");
      case "takeOver": return void model.takeOver(props.task);
      case "cancel": return void model.cancel(props.task);
    }
  };
  const submit = async () => {
    const which = form();
    if (!which || !reason().trim()) return;
    const outcome = await model.submit(props.task, which, reason(), returnLater());
    if (outcome && outcome !== "cancelled" && outcome.outcome === "done") setForm(undefined);
  };
  return (
    <div class="team-task-actions" data-task={props.task.id}>
      <Show when={props.task.no_report}>
        <div class="team-no-report" data-no-report={props.task.id}>
          <div class="team-no-report-head">
            <Icon name="message-square" size={13} />
            <b>{tr("No report")}</b>
            <span class="dim">{tr("The member's last reply")}</span>
            <span class="grow" />
            <Show when={reply()}>
              <button type="button" class="btn small ghost" data-action="reply-more" aria-expanded={replyOpen()} onClick={() => setReplyOpen(!replyOpen())}>
                {replyOpen() ? tr("Show less") : tr("Show more")}
              </button>
            </Show>
          </div>
          <Show when={reply()} fallback={<p class="pm-empty">{tr("No reply was kept for this attempt")}</p>}>
            <p class={replyOpen() ? "team-no-report-text open" : "team-no-report-text"}>{reply()}</p>
          </Show>
          <div class="team-no-report-actions">
            <button type="button" class="btn small primary" data-action="use-last-reply" disabled={read().busy || !props.task.attempt_id}
              onClick={() => void model.useLastReply(props.task)}>{tr("Use last reply as report")}</button>
            <button type="button" class="btn small" data-action="remind-member" disabled={read().busy} onClick={() => void remind()}>{tr("Remind")}</button>
            <button type="button" class="btn small" data-action="retry-no-report" disabled={read().busy} onClick={() => void model.retry(props.task)}>{tr("Retry")}</button>
          </div>
          <Show when={remindNotice()}><div class="team-proposal-notice" role="status">{remindNotice()}</div></Show>
        </div>
      </Show>
      <Show when={(props.task.questions ?? []).length > 0}>
        <div class="team-task-questions">
          <b>{tr("Questions")}</b>
          <For each={props.task.questions ?? []}>{(question) => <QuestionAnswer computer={props.computer} team={props.team} task={props.task} question={question} />}</For>
        </div>
      </Show>
      <Show when={props.task.pipeline_run}>{(run) => (
        <div class="team-stage-chips" aria-label={tr("Pipeline %1", run().name || props.task.pipeline || "")}>
          <For each={run().stages}>{(kind, index) => (
            <span class="team-chip" classList={{ accent: run().open?.stage === index() }} data-stage={kind}>{stageName(kind)}</span>
          )}</For>
        </div>
      )}</Show>
      <Show when={!props.task.subteam && (props.team.status === "running" || props.team.status === "paused")}>
        <div class="team-task-delegate">
          <button type="button" class="btn ghost small" data-task-action="delegate" onClick={() => setDelegate(true)}>{tr("Delegate to a sub-team…")}</button>
        </div>
      </Show>
      <Show when={delegate()}>
        <TextDialog title={tr("Delegate to a sub-team")} hint={tr("Create a sub-team from task #%1? Its lead will be the assignee.", props.task.id)}
          placeholder={tr("Sub-team name")} action={tr("Delegate")} close={() => setDelegate(false)}
          submit={(name) => void model.createSubteam(name, props.task.id)} />
      </Show>
      <Show when={words()}>{(text) => (
        <div class="team-task-form" data-form={form()}>
          <b>{text().heading}</b>
          <p class="dim">{text().what}</p>
          <TextArea class="field" rows={3} placeholder={text().placeholder} value={reason()} onInput={(event) => setReason(event.currentTarget.value)} ref={(el) => queueMicrotask(() => el.focus())} />
          <Show when={form() === "reject"}>
            <label class="team-proposal-option"><input type="checkbox" checked={returnLater()} onChange={(event) => setReturnLater(event.currentTarget.checked)} />
              {tr("Return later (hold): keep it with the member until released")}</label>
          </Show>
          <div class="team-proposal-answers"><span class="grow" />
            <button type="button" class="btn small" onClick={() => setForm(undefined)}>{tr("Cancel")}</button>
            <button type="button" class="btn small primary" disabled={!reason().trim() || read().busy} onClick={() => void submit()}>{text().submit}</button>
          </div>
        </div>
      )}</Show>
      <div class="team-task-footer">
        <For each={taskButtons(props.task)}>{(b) => (
          <button type="button" class="btn small" classList={{ primary: b.primary, danger: b.danger }} data-action={b.id} disabled={read().busy}
            onClick={() => (b.form ? open(b.form) : button(b.id))}>{b.text}</button>
        )}</For>
        <span class="grow" />
        <button type="button" class="btn small ghost" aria-haspopup="menu" aria-expanded={menu() === "more"} onClick={() => setMenu(menu() === "more" ? undefined : "more")}>
          {tr("More")}<Icon name="chevron-down" size={12} />
        </button>
      </div>
      <Show when={menu() === "more"}>
        <div class="menu team-task-menu" role="menu" aria-label={tr("More task actions")}>
          <For each={moreItems(props.team, props.task)}>{(entry) => (
            <button type="button" role="menuitem" class="menu-item" disabled={!entry.enabled} title={entry.why} data-item={entry.id}
              onClick={() => item(entry.id, entry.phase, entry.form)}>{entry.text}</button>
          )}</For>
        </div>
      </Show>
      <Show when={menu() === "reassign"}>
        <div class="menu team-task-menu" role="menu" aria-label={tr("Reassign to")}>
          <For each={peers()} fallback={<span class="dim">{tr("No other member")}</span>}>{(member) => (
            <button type="button" role="menuitem" class="menu-item" onClick={() => { setMenu(undefined); void model.reassign(props.task, member.id); }}>
              {member.role || member.id}
            </button>
          )}</For>
        </div>
      </Show>
      <Show when={read().notice}><div class="team-proposal-notice" role="status">{read().notice}</div></Show>
    </div>
  );
}

/**
 * Task 55: a member's question with the owner's answer (Qt teamTaskQuestion):
 * the field keeps its text across polls, Send goes once and waits for hgs, a
 * refusal says why here, and a closed question says so without a field.
 */
export function QuestionAnswer(props: { computer: ComputerId; team: ActTeam; task: ActTask; question: { id: string; text: string } }) {
  const { model, tick } = useModel((changed) => new TeamActions(windowPort, () => ({ computer: props.computer, team: props.team }), changed));
  const read = () => (tick(), model);
  const [draft, setDraft] = createSignal("");
  const [sent, setSent] = createSignal(false);
  const assignee = () => props.team.members.find((member) => member.id === props.task.assignee)?.role || props.task.assignee;
  const closed = () => questionClosed(props.task);
  const send = async () => {
    if (sent() || read().busy || !draft().trim()) return;
    const outcome = await model.reply(props.task, props.question.id, draft());
    if (outcome && outcome !== "cancelled" && outcome.outcome === "done") { setSent(true); setDraft(""); }
  };
  return (
    <div class="team-question" data-question={props.question.id}>
      <p class="team-question-text">{props.question.text}</p>
      <Show when={!closed()} fallback={<p class="dim team-question-closed">{closed()}</p>}>
        <Show when={!sent()} fallback={<p class="dim team-question-sent">{tr("Answer sent; it reaches %1 once.", assignee())}</p>}>
          <div class="team-proposal-answers">
            <TextInput class="field" maxlength={4000} placeholder={tr("Answer %1…", assignee())} value={draft()} disabled={read().busy}
              onInput={(event) => setDraft(event.currentTarget.value)} onKeyDown={(event) => { if (event.key === "Enter") void send(); }} />
            <button type="button" class="btn small primary" disabled={read().busy || !draft().trim()}
              title={tr("Send this answer to %1; it reaches the member as a message", assignee())} onClick={() => void send()}>
              {read().busy ? tr("Sending…") : tr("Send")}
            </button>
          </div>
          <Show when={read().notice}><p class="team-proposal-notice" role="status">{tr("Not sent: %1", read().notice)}</p></Show>
        </Show>
      </Show>
    </div>
  );
}

/** Task 55: the members' open questions on the team's side, each answered in place. */
export function TeamQuestions(props: { computer: ComputerId; team: ActTeam }) {
  const keys = createMemo(() => openQuestions(props.team).map((entry) => `${entry.task.id}:${entry.question.id}`));
  const entry = (key: string) => openQuestions(props.team).find((found) => `${found.task.id}:${found.question.id}` === key);
  return (
    <Show when={keys().length > 0}>
      <div class="team-questions" role="list" aria-label={tr("Questions")}>
        <For each={keys()}>{(key) => (
          <Show when={entry(key)}>{(found) => (
            <div class="team-question-row" role="listitem">
              <span class="dim">{tr("#%1 %2 asks:", found().task.id, props.team.members.find((member) => member.id === found().task.assignee)?.role || found().task.assignee)}</span>
              <QuestionAnswer computer={props.computer} team={props.team} task={found().task} question={found().question} />
            </div>
          )}</Show>
        )}</For>
      </div>
    </Show>
  );
}

/** Show what blocks: the open tasks that wait, and what each waits for (Qt teamShowBlockers). */
export function Blockers(props: { team: ActTeam; onTask?: (id: number) => void }): JSX.Element {
  const [shown, setShown] = createSignal(false);
  const waiting = createMemo(() => props.team.tasks.filter((task) => !["accepted", "rejected", "cancelled"].includes(task.status) && task.waits));
  return (
    <span class="team-blockers">
      <button type="button" class="btn small ghost" aria-expanded={shown()} onClick={() => setShown(!shown())}>{shown() ? tr("Hide blockers") : tr("Show what blocks")}</button>
      <Show when={shown()}>
        <span class="team-blockers-list">
          <For each={waiting()} fallback={<span class="dim">{tr("Nothing names a blocker.")}</span>}>{(task) => (
            <button type="button" class="team-chip link" onClick={() => props.onTask?.(task.id)}>{tr("#%1 waits: %2", task.id, task.waits ?? "")}</button>
          )}</For>
        </span>
      </Show>
    </span>
  );
}
