import { For, Show, createEffect, createSignal } from "solid-js";
import { Icon } from "../lib/icons";
import * as organization from "../../store/organization";
import type { MenuEntry } from "../../store/ui";
import type { Session } from "./types";
import { createOrgDialogs, deleteText, nameProblem, projectColor, type OrgDialog } from "./organization-ui";
import { tr } from "../../i18n";

// The organization dialogs: Move to project (also for a selection), New
// project, Rename project and Delete project, in one modal host as
// ActionDialogs. Their rules and calls are organization-ui.ts's.

const dialogs = createOrgDialogs(organization);
const { dialog, close } = dialogs;

export const openMoveDialog = (rows: readonly Session[]): void => dialogs.openMove(rows);
export const openCreateProjectDialog = (): void => dialogs.openNew();
export const openRenameProjectDialog = (project: string): void => dialogs.openRename(project);
export const openDeleteProjectDialog = (project: string): void => dialogs.openDelete(project);
/** The session menu's Move to project entry. */
export const moveEntry = (s: Session): MenuEntry => dialogs.moveEntry(s);

function MoveDialog(props: { rows: Session[] }) {
  return (
    <>
      <div class="dlg-head"><Icon name="folder" size={18} class="accent" /><h2 id="org-dlg-title">{tr("Move to project")}</h2></div>
      <div class="dlg-body col" style={{ gap: "4px" }}>
        <For each={dialogs.moveChoicesFor(props.rows)}>{(p) => (
          <button type="button" class="menu-item entity" style={{ "--entity": projectColor(p) }}
            aria-pressed={p.current} onClick={() => dialogs.choose(p.id)}>
            <Icon name="folder" size={14} /><span class="ell">{p.name}</span>
            <Show when={p.current}><span class="grow" /><Icon name="check" size={14} /></Show>
          </button>
        )}</For>
        <div class="menu-sep" />
        <button type="button" class="menu-item" onClick={dialogs.newProject}><Icon name="plus" size={14} />{tr("New project…")}</button>
      </div>
      <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn" data-initial onClick={close}>{tr("Cancel")}</button></div>
    </>
  );
}

function NameDialog(props: { title: string; initial: string }) {
  const [value, setValue] = createSignal(props.initial);
  const done = (e: Event) => { e.preventDefault(); dialogs.submitName(value()); };
  return (
    <form onSubmit={done}>
      <div class="dlg-head"><Icon name="folder" size={18} class="accent" /><h2 id="org-dlg-title">{props.title}</h2></div>
      <div class="dlg-body">
        <label class="dlg-label" for="org-name">{tr("Project name")}</label>
        <input id="org-name" data-initial class="dlg-input" value={value()} onInput={(e) => setValue(e.currentTarget.value)} />
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn" onClick={close}>{tr("Cancel")}</button>
        <button type="submit" class="btn primary" disabled={!!nameProblem(value())}>{tr("OK")}</button>
      </div>
    </form>
  );
}

function DeleteDialog(props: Extract<OrgDialog, { kind: "delete" }>) {
  const [target, setTarget] = createSignal(props.selected);
  return (
    <>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="org-dlg-title">{tr("Delete project")}</h2></div>
      <div class="dlg-body">
        <p class="confirm-body" id="org-dlg-body">{deleteText(props.name)}</p>
        <select class="field" style={{ width: "100%", "margin-top": "12px" }} aria-label={tr("Project that receives the sessions")} value={target()} onChange={(e) => setTarget(e.currentTarget.value)}>
          <For each={props.options}>{(p) => <option value={p.id}>{p.name}</option>}</For>
        </select>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn solid-danger" disabled={!target()} onClick={() => dialogs.confirmDelete(target())}>{tr("Move sessions and delete")}</button>
        <button type="button" class="btn primary" data-initial autofocus onClick={close}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/**
 * One modal host. In one code path it opens the modal, then focuses the
 * dialog's initial control: the name with its text selected, or Cancel. In
 * Move to project the check mark shows the current project, and Cancel takes
 * the focus, so a repeated Enter never moves the rows within their own project.
 * Escape closes through the dialog's own cancel.
 */
export function OrganizationDialogs() {
  let el: HTMLDialogElement | undefined;
  createEffect(() => {
    const state = dialog();
    if (!el) return;
    if (!state) { if (el.open) el.close(); return; }
    if (!el.open) el.showModal();
    queueMicrotask(() => {
      const initial = el?.querySelector<HTMLElement>("[data-initial]");
      initial?.focus();
      if (initial instanceof HTMLInputElement) initial.select();
    });
  });
  const kind = () => dialog()?.kind;
  return (
    <dialog class="dlg" ref={el} role={kind() === "delete" ? "alertdialog" : "dialog"} aria-labelledby="org-dlg-title"
      aria-describedby={kind() === "delete" ? "org-dlg-body" : undefined} style={{ width: kind() === "move" ? "380px" : "460px" }} onClose={close}>
      <Show when={dialog()} keyed>
        {(d) => {
          if (d.kind === "move") return <MoveDialog rows={d.rows} />;
          if (d.kind === "delete") return <DeleteDialog {...d} />;
          const named = dialogs.nameDialog();
          return <NameDialog title={named?.title ?? tr("New project")} initial={named?.initial ?? ""} />;
        }}
      </Show>
    </dialog>
  );
}
