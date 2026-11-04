import { For, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { Json } from "../lib/json";
import type { Style } from "./paths";
import { WorktreePicker, type PickerSession } from "./worktree-dialogs";
import { tr } from "../../i18n";

// Choose existing… (NewSessionDialog::chooseWorktree 1114-1124): a dialog
// with WorktreePanel in picker mode (tray/src/WorktreePanel.h), from the
// prototype's layout (design/webui/src/dialogs/setup/NewSession.tsx,
// ChooseWorktree) where Qt does not differ. No All sessions, Clean up or
// context menu, and nothing here creates or removes anything: it reads the
// folder's catalog and answers the chosen folder. Its buttons are Qt's
// non-default ones: Enter on them does nothing.

/** The panel's reads, bound to its computer and folder. */
export interface WorktreeReads { snapshot(): Promise<Json>; catalog(refresh: boolean): Promise<Json> }

export function ChooseWorktreeDialog(props: {
  /** The computer as the status names it. */
  machine: string;
  path: string;
  reads: WorktreeReads;
  online: () => boolean;
  style: () => Style;
  /** The computer's listed sessions, for each row's count. */
  sessions: () => readonly PickerSession[];
  onUse(path: string): void;
  onClose(): void;
}) {
  let el!: HTMLDialogElement;
  let tree!: HTMLUListElement;
  let gone = false;
  const panel = new WorktreePicker(props.reads, props.path, props.machine);
  const [version, setVersion] = createSignal(0);
  panel.onChange = () => setVersion((v) => v + 1);
  const v = createMemo(() => { version(); return panel.view(props.online(), props.sessions(), props.style()); });

  onMount(() => {
    el.showModal();
    // Shown: the catalog is read (showEvent).
    panel.show(props.online());
    queueMicrotask(() => tree.focus());
  });
  onCleanup(() => {
    gone = true;
    panel.dispose();
    if (el.open) el.close();
  });
  createEffect(() => {
    const at = v().selected;
    if (at !== undefined) tree.querySelector(`[data-path="${CSS.escape(at)}"]`)?.scrollIntoView({ block: "nearest" });
  });

  const use = () => {
    const path = v().selected;
    if (!v().useEnabled || path === undefined) return;
    props.onUse(path);
    props.onClose();
  };
  const treeKey = (e: KeyboardEvent) => {
    const rows = v().rows;
    if (!rows.length) return;
    const at = rows.findIndex((row) => row.path === v().selected);
    const to = e.key === "ArrowDown" ? at + 1 : e.key === "ArrowUp" ? at - 1 : e.key === "Home" ? 0 : e.key === "End" ? rows.length - 1 : undefined;
    if (to === undefined) return;
    e.preventDefault();
    panel.select(rows[Math.max(0, Math.min(rows.length - 1, to))].path);
  };
  const keys = (e: KeyboardEvent) => {
    if (e.key === "Enter" && (e.target as HTMLElement).tagName === "BUTTON") e.preventDefault();
  };

  return (
    <dialog class="dlg wt-dlg" ref={el} aria-labelledby="wt-title" onKeyDown={keys}
      onCancel={(e) => { e.preventDefault(); props.onClose(); }} onClose={() => { if (!gone) props.onClose(); }}>
      <div class="dlg-head"><h2 id="wt-title">{tr("Choose an existing worktree")}</h2></div>
      <div class="dlg-body wt-body">
        <div class="row">
          <h3 class="section-title grow" id="wt-heading">{tr("Folder and worktrees")}</h3>
          <button type="button" class="btn small" disabled={!v().refreshEnabled} onClick={() => panel.refresh(props.online())}>{tr("Refresh")}</button>
        </div>
        <p class="wt-status" title={v().status}>{v().status}</p>
        <ul class="wt-tree" ref={tree} role="listbox" aria-labelledby="wt-heading" tabindex="0"
          aria-activedescendant={v().selected !== undefined ? `wt-o-${v().rows.findIndex((r) => r.path === v().selected)}` : undefined} onKeyDown={treeKey}>
          <For each={v().rows}>{(row, index) => (
            <li id={`wt-o-${index()}`} role="option" data-path={row.path} aria-selected={row.path === v().selected} classList={{ on: row.path === v().selected, off: !row.usable }}
              title={row.tooltip} onClick={() => panel.select(row.path)}>
              <span class="ell">{row.label}</span>
              <span class="mono ell wt-path">{row.path}</span>
            </li>
          )}</For>
        </ul>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn primary" disabled={!v().useEnabled} onClick={use}>{tr("Use folder")}</button>
        <button type="button" class="btn" onClick={() => props.onClose()}>{tr("Cancel")}</button>
      </div>
    </dialog>
  );
}
