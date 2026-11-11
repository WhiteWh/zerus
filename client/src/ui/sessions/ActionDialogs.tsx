import { TextInput } from "../lib/TextField";
import { For, Show, createEffect, createMemo, createSignal, lazy, onCleanup, onMount, untrack } from "solid-js";
import { Icon } from "../lib/icons";
import { catalog, computerOffline } from "../../store/catalog";
import { GONE, change, copyCommand, copyFolder, copyName, copySessionId, copyReference, end, forget, interrupting, notify, perform, stillListed, pendingNotifications, toggleMute, updating } from "../../store/actions";
import { bindBatchConfirm, endAfter, running } from "../../store/batch";
import { childOfferFor } from "../../store/endChildren";
import { bindResumeTerminalConfirm } from "../../store/sessionTerminal";
import { cancelRestoreFollow, forgetArchived, refreshArchiveWhenDone, restoreSession } from "../../store/restores";
import type { MenuEntry } from "../../store/ui";
import type { Details, Outcome } from "../../types";
import type { Session } from "./types";
import { FRESH_CHANGED, STATE_CHANGED, archiveNameOccupied, canStartStopped, FRESH_TIP, copyState, dialogReducer, freshCopy, freshEntry, freshStillValid, menuState, notificationsState, renameParts, renameProblem, forgetArchivedCopy, forgetCopy, forgetOpening, snapshotOf, terminateCopy, terminates, type ForgetScope, type ActionRow, type DialogEvent } from "./actions-ui";
import type { BatchConfirmation, SelectionRow } from "./selection";
import { childChoiceText, childHintText, childStayText, type ChildAction, type ChildOffer } from "./end-children";
import { terminationCleanupOffer, terminationCleanupCancel } from "../../bridge";
import { targetOf } from "../../store/actions";
import { setSetting } from "../../store/settings";
import { sessionForks } from "../../store/sessionForks";
import { FORK_CHECKING, FORK_HINT } from "./fork-ui";
import { tr } from "../../i18n";
import { readCleanupOffer, type CleanupProgram } from "./termination-offer";
import { TerminationPrograms } from "./TerminationPrograms";
import { CleanupOfferNotice } from "./CleanupOfferNotice";
import { captureUntrackedShell, untrackedShell, UNVERIFIED_SHELL } from "../../store/untrackedShell";
import type { Action } from "../../types";

const ForkDialog = lazy(() => import("./ForkDialog"));

// The guarded session actions: the entries both menus share, the Rename,
// Terminate, Forget (saved or archived), Start fresh and selection dialogs.
// Their notices go to the window's notice bus (store/notices.ts). The
// Terminate and Move to archive confirmations of a parent with live child
// sessions also offer to end those, off by default (zerus-sa9qn.17); Move to
// archive asks only then. An archived row's change entry is
// Restore session (store/restores.ts). While a selection batch runs
// (store/batch.ts, Qt's m_pending) every write entry here is off. A
// successful Rename or Move to archive reloads the archive pages (Qt's
// writeDone, SessionsWindow.cpp 1917).

const [open, dispatch] = createSignal<ReturnType<typeof dialogReducer>>(undefined, { equals: false });
const send = (event: DialogEvent) => dispatch(dialogReducer(open(), event));
const closeDialog = () => send({ type: "close" });

// Resume and open terminal asks once here; any way out but the confirm button is a Cancel.
bindResumeTerminalConfirm((row) => new Promise<boolean>((resolve) => {
  let answered = false;
  const answer = (confirmed: boolean) => { if (!answered) { answered = true; resolve(confirmed); } };
  send({ type: "open", kind: "resume_terminal", row, answer });
}));

// A selection's Forget or Terminate asks here; any way out but the confirm button is a Cancel.
bindBatchConfirm((copy) => new Promise<boolean>((resolve) => {
  let answered = false;
  const answer = (confirmed: boolean) => { if (!answered) { answered = true; resolve(confirmed); } };
  send({ type: "open", kind: "batch", copy, answer });
}));

type Item = Extract<MenuEntry, { label: string }> & { action: () => void };

/**
 * The action entries in menu order. `details` only for the selected session
 * (the header); `context` for a row's context menu. The entries act on a
 * snapshot of the row as it is now, which the store checks against the
 * listed row when they run.
 */
export function actionEntries(s: Session, details?: Details, options: { context?: boolean } = {}): { change: Item; stop: Item & { shown: boolean; interrupt: () => Promise<Outcome | undefined> }; rename: Item; fork: Item; archive?: Item; end: Item } {
  const row = snapshotOf(s);
  const archived = row.state === "archived";
  const m = menuState(row, !computerOffline(s.computer), updating(s.computer, s.name), details, {
    context: options.context, interrupting: interrupting(s.computer, s.name), batchRunning: running(),
    // Qt's archiveNameOccupied: one scan of the live rows, for an archived row only.
    occupied: archived && archiveNameOccupied(row, catalog.rows),
  });
  const hint = (enabled: boolean, reason?: string) => (enabled || !reason ? undefined : tr(reason));
  // The Stop's interrupt, whose outcome the composer reads (zerus-5wmh.13.4).
  const interrupt = () => details ? perform(row, { kind: "interrupt", turn_started: details.turn_started }) : Promise.resolve(undefined);
  return {
    change: {
      label: tr(m.change.label), icon: archived || row.state !== "running" ? "play" : "pause", disabled: !m.change.enabled, hint: hint(m.change.enabled, m.change.reason),
      action: () => { void (archived ? restoreSession(row) : change(row)); },
    },
    stop: {
      label: tr("Stop"), icon: "circle-stop", shown: m.stop.visible, disabled: !m.stop.enabled, hint: hint(m.stop.enabled, m.stop.reason),
      interrupt, action: () => { void interrupt(); },
    },
    rename: { label: tr("Rename…"), icon: "pencil", disabled: !m.rename, action: () => send({ type: "open", kind: "rename", row }) },
    fork: forkEntry(s, details),
    archive: m.archive.visible
      ? { label: tr("Move to archive"), icon: "archive", disabled: !m.archive.enabled, hint: hint(m.archive.enabled, m.archive.reason), action: () => {
        // A parent with live child sessions confirms first and may end them too.
        const children = childOfferFor(row, "archive");
        if (children.ending.length || children.staying.length) { send({ type: "open", kind: "archive", row }); return; }
        void perform(row, { kind: "archive" }).then(refreshArchiveWhenDone);
      } }
      : undefined,
    end: {
      label: tr(m.end.label), icon: m.end.label === "Terminate session…" ? "circle-stop" : "trash", danger: true, disabled: !m.end.enabled, hint: hint(m.end.enabled, m.end.reason),
      action: () => {
        // Qt's click-time GONE pre-check first; a forget names the run and conversation in view.
        if (terminates(row, details)) { send({ type: "open", kind: "terminate", row }); return; }
        const live = stillListed(row);
        if (!live) return;
        // An archived row is keyed by its archive id; Qt's key also holds the name.
        if (archived && live.name !== row.name) { notify("info", tr(GONE), true); return; }
        const opening = forgetOpening(row, live, details);
        if (opening === "changed") { notify("error", tr(STATE_CHANGED), true); return; }
        send({ type: "open", kind: "forget", ...opening });
      },
    },
  };
}

/** Both menus share the inspected source and the one guarded Fork path. */
export function forkEntry(s: Session, details?: Details): Item {
  const row = snapshotOf(s);
  return { label: tr("Fork session…"), icon: "git-fork", disabled: !sessionForks.available(row, details),
    hint: details ? details.fork_reason || tr(FORK_HINT) : tr(FORK_CHECKING),
    action: () => { if (details && sessionForks.available(row, details)) send({ type: "open", kind: "fork", row, details: { ...details } }); },
  };
}

/**
 * The copy entries (Qt's context menu, 5306-5307 and 5374-5376). `details`
 * only for the selected session, whose folder and run they prefer. They
 * show no hint when off, as Qt shows none.
 */
export function copyEntries(s: Session, details?: Details): { command: Item; id: Item; name: Item; reference: Item; folder: Item } {
  const row = snapshotOf(s);
  const c = copyState(row, !computerOffline(s.computer), details);
  return {
    command: { label: tr(c.command.label), icon: "link", disabled: !c.command.enabled, action: () => { void copyCommand(row); } },
    id: { label: tr("Copy session ID"), icon: "copy", disabled: !row.conversationId, hint: row.conversationId ? undefined : tr("This session has no recorded conversation ID."), action: () => { void copySessionId(row); } },
    name: { label: tr("Copy session name"), icon: "copy", action: () => { void copyName(row); } },
    reference: { label: tr("Copy session reference"), icon: "link", disabled: !c.reference, action: () => { void copyReference(row, details); } },
    folder: { label: tr("Copy folder path"), icon: "copy", disabled: !c.folder.enabled, action: () => { void copyFolder(row, details); } },
  };
}

/**
 * The Mute or Unmute entry of a row's context menu (Qt's contextToggleMute),
 * undefined for an archived row. Its hint is always shown: the reason it is
 * off, else what it does.
 */
export function muteEntry(s: Session): Item | undefined {
  const row = snapshotOf(s);
  const m = notificationsState(row, !computerOffline(s.computer), pendingNotifications(s.computer, s.conversationId));
  if (!m) return undefined;
  return { label: tr(m.label), icon: row.muted ? "bell" : "bell-off", disabled: !m.enabled, hint: tr(m.hint), action: () => { void toggleMute(row); } };
}

/**
 * The Start fresh entry of a paused or stopped row (Qt's contextStartFresh and
 * the header's m_freshAction), undefined for other rows. Clicking runs the
 * click-time recheck, then asks.
 */
export function freshActionEntry(s: Session, header = false): Item | undefined {
  const row = snapshotOf(s);
  const m = freshEntry(row, !computerOffline(s.computer), updating(s.computer, s.name), running());
  if (!m) return undefined;
  return {
    label: tr("Start fresh…"), icon: "clear", disabled: !m.enabled, hint: header ? tr(FRESH_TIP) : m.hint && tr(m.hint),
    action: () => {
      const live = stillListed(row);
      // Qt returns silently when the session can no longer start fresh.
      if (live && canStartStopped(live, !computerOffline(live.computer), updating(live.computer, live.name), running())) send({ type: "open", kind: "fresh", row: snapshotOf(live) });
    },
  };
}

/** The listed rows, for the rename dialog's check of names in use. */
function* listedRows(): Iterable<Session> {
  for (const id of catalog.order) {
    const row = catalog.rows[id];
    if (row) yield row;
  }
}

/**
 * The Rename dialog's call (Qt's renameSession, then writeDone): a rename
 * cancels a pending restore's follow (Qt clears m_restoreKey) and a success
 * reloads the archive pages (1917).
 */
export function renameSession(row: ActionRow, newName: string): Promise<Outcome> {
  cancelRestoreFollow();
  return perform(row, { kind: "rename", new_name: newName, archive_id: row.archiveId ?? null }).then(refreshArchiveWhenDone);
}

/** Qt's rename dialog: the agent and project stay; only the tag after them is edited. */
function RenameDialog(props: { row: ActionRow; close: () => void }) {
  const { prefix, tag: oldTag } = renameParts(props.row.name);
  const [value, setValue] = createSignal(oldTag);
  // Checked as the text changes; the listed names are read then, not followed.
  const problem = createMemo(() => {
    const tag = value();
    return untrack(() => renameProblem(tag, props.row, listedRows()));
  });
  const submit = (e?: Event) => {
    e?.preventDefault();
    const tag = value();
    if (tag === oldTag || renameProblem(tag, props.row, listedRows())) return;
    props.close();
    void renameSession(props.row, prefix + tag);
  };
  return (
    <form onSubmit={submit}>
      <div class="dlg-head"><Icon name="pencil" size={18} class="accent" /><h2 id="action-dlg-title">{tr("Rename session")}</h2></div>
      <div class="dlg-body">
        <div class="dlg-detail dlg-context">{props.row.machine} / {prefix.slice(0, -1)}</div>
        <label class="dlg-label" for="rename-tag">{tr("Session name")}</label>
        <TextInput id="rename-tag" data-initial class="dlg-input" placeholder={tr("e.g. release planning")} value={value()} onInput={(e) => setValue(e.currentTarget.value)} />
        <div class="dlg-detail ell" title={prefix + value()}>{prefix + value()}</div>
        <Show when={problem()}><div class="dlg-error">{problem()}</div></Show>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button>
        <button type="submit" class="btn primary" disabled={!!problem() || value() === oldTag}>{tr("Rename")}</button>
      </div>
    </form>
  );
}

export function TerminateDialog(props: { row: ActionRow; close: () => void }) {
  const copy = terminateCopy(props.row);
  const shell = untrackedShell(props.row);
  const [terminal, setTerminal] = createSignal<Extract<Action, { kind: "terminate_shell" }>>();
  const [terminalError, setTerminalError] = createSignal("");
  const [offer, setOffer] = createSignal<string>();
  const [remove, setRemove] = createSignal(false);
  const [programs, setPrograms] = createSignal<readonly CleanupProgram[]>([]);
  const [offerError, setOfferError] = createSignal("");
  // The live child sessions as the dialog opened; a verified shell has none.
  const children = shell ? { ending: [], staying: [] } : untrack(() => childOfferFor(props.row, "terminate"));
  const [alsoChildren, setAlsoChildren] = createSignal(false);
  let closed = false, consumed = false;
  onMount(() => {
    if (shell) {
      void captureUntrackedShell(props.row).then((action) => { if (!closed) setTerminal(action); })
        .catch(() => { if (!closed) setTerminalError(UNVERIFIED_SHELL); });
      return;
    }
    const target = targetOf(props.row, "terminate");
    if (copy.action !== "terminate" || !target || !target.conversation_id || props.row.gitWorktree === false) return;
    void readCleanupOffer(target, { offer: terminationCleanupOffer, cancel: terminationCleanupCancel }, () => !closed, (state) => {
      setOffer(state.offer?.id); setRemove(state.offer?.checked ?? false); setOfferError(state.error ?? "");
      setPrograms(state.offer?.programs ?? []);
    });
  });
  onCleanup(() => { closed = true; const id = offer(); if (id && !consumed) void terminationCleanupCancel(id); });
  const submit = async () => {
    if (shell && !terminal()) return;
    const chosen = alsoChildren() ? children.ending : [];
    const id = remove() ? offer() : undefined;
    const released = !id ? offer() : undefined;
    consumed = !!id || !!released;
    if (offer() && !remove()) void setSetting("workspace/removeWorktreeAfterTermination", false);
    props.close();
    if (released) {
      try { await terminationCleanupCancel(released); }
      catch (error) { notify("error", String(error), true); void endAfter("terminate", props.row, undefined, chosen); return; }
    }
    // The children only after this session's own outcome, never with its folder choice.
    void end(props.row, terminal() ?? copy.action, id).then((out) => endAfter("terminate", props.row, out, chosen));
  };
  return (
    <>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="action-dlg-title">{copy.title}</h2></div>
      <div class="dlg-body">
        <div class="confirm-body" id="action-dlg-body">{shell ? tr("Close the verified shell terminal for %1? Its working folder will be kept; no archive will be created.", props.row.name) : copy.text}</div>
        <div class="dlg-detail">{copy.detail}</div>
        <Show when={shell}>
          <p class="hint">{tr("The working folder will be kept. Detached jobs may continue after this shell terminal closes.")}</p>
          <Show when={terminalError()}><p class="dlg-error">{tr(terminalError())}</p></Show>
        </Show>
        <Show when={offerError()}><CleanupOfferNotice error={offerError()} review={props.row.cwdPath ? () => {
          // Opening the existing review neither terminates nor removes anything.
          const { computer, cwdPath } = props.row;
          props.close();
          void import("../../store/worktreeCleanup").then(({ openWorktreeReview }) => openWorktreeReview(computer, cwdPath, cwdPath))
            .catch((error) => notify("error", String(error), true));
        } : undefined} /></Show>
        <Show when={offer()}>
          <label class="check" style={{ "margin-top": "10px" }}><input type="checkbox" checked={remove()} onChange={(e) => setRemove(e.currentTarget.checked)} /> {tr("Remove the working folder after stopping")}</label>
          <Show when={remove()}><TerminationPrograms programs={programs()} /></Show>
          <p class="hint" style={{ "margin-top": "6px" }}>{tr("Removal runs in the background after exact successful termination, only for a verified generated worktree. Unknown local data keeps the folder for review. Computer: %1", props.row.machine)}</p>
        </Show>
        <ChildSessions action="terminate" offer={children} checked={alsoChildren()} onChange={setAlsoChildren} />
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn solid-danger" onClick={submit} disabled={shell && !terminal()}>{offer() ? remove() ? tr("Stop and remove folder") : tr("Stop and archive") : copy.confirm}</button>
        <button type="button" class="btn primary" data-initial autofocus onClick={props.close}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/** The most child sessions a confirmation names one by one; the rest are counted. */
const CHILDREN_SHOWN = 50;

/**
 * A parent's live child sessions in its Terminate or Archive confirmation
 * (zerus-sa9qn.17): the opt-in, off by default, with the children the action
 * would end, and the children it cannot end now, which stay as they are.
 */
function ChildSessions(props: { action: ChildAction; offer: ChildOffer; checked: boolean; onChange: (checked: boolean) => void }) {
  const names = (rows: readonly SelectionRow[]) => (
    <ul class="dlg-children" style={{ "max-height": "140px", overflow: "auto", margin: "4px 0 6px", "padding-left": "24px", "list-style": "none", color: "var(--z-color-dim)", "font-size": "var(--fs-sm)" }}>
      <For each={rows.slice(0, CHILDREN_SHOWN)}>{(s) => <li dir="auto" class="ell" title={s.name}>{s.name}</li>}</For>
      <Show when={rows.length > CHILDREN_SHOWN}><li>{tr("+%1 more", rows.length - CHILDREN_SHOWN)}</li></Show>
    </ul>
  );
  return (
    <Show when={props.offer.ending.length + props.offer.staying.length > 0}>
      <section class="dlg-child-sessions" aria-label={tr("Child sessions")}>
        <Show when={props.offer.ending.length > 0}>
          <label class="check" style={{ "margin-top": "10px" }}><input type="checkbox" checked={props.checked} onChange={(e) => props.onChange(e.currentTarget.checked)} /> {childChoiceText(props.action, props.offer.ending.length)}</label>
          {names(props.offer.ending)}
          <p class="hint">{childHintText(props.action)}</p>
        </Show>
        <Show when={props.offer.staying.length > 0}>
          <p class="hint" style={{ "margin-top": "8px" }}>{childStayText(props.action, props.offer.staying.length)}</p>
          {names(props.offer.staying)}
        </Show>
      </section>
    </Show>
  );
}

/**
 * Move to archive of a parent with live child sessions (zerus-sa9qn.17): the
 * archive runs as from the menu, then, only when chosen, each listed child
 * through its own Move to archive.
 */
export function ArchiveDialog(props: { row: ActionRow; close: () => void }) {
  const children = untrack(() => childOfferFor(props.row, "archive"));
  const [alsoChildren, setAlsoChildren] = createSignal(false);
  const submit = () => {
    const chosen = alsoChildren() ? children.ending : [];
    props.close();
    void perform(props.row, { kind: "archive" }).then(refreshArchiveWhenDone).then((out) => endAfter("archive", props.row, out, chosen));
  };
  return (
    <>
      <div class="dlg-head"><Icon name="archive" size={18} class="accent" /><h2 id="action-dlg-title">{tr("Move to archive?")}</h2></div>
      <div class="dlg-body">
        <div class="confirm-body" id="action-dlg-body">{tr("Move %1 to Archive? Its live child sessions stay listed under it until they end too.", props.row.name)}</div>
        <div class="dlg-detail">{tr("Computer: %1", props.row.machine)}</div>
        <ChildSessions action="archive" offer={children} checked={alsoChildren()} onChange={setAlsoChildren} />
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn" onClick={submit}>{tr("Move to archive")}</button>
        <button type="button" class="btn primary" data-initial autofocus onClick={props.close}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/**
 * Forget saved session or Forget archived session (Qt terminateSession): the
 * confirm button acts only on the instance the dialog named.
 */
function ForgetDialog(props: { row: ActionRow; bound: ForgetScope; close: () => void }) {
  const archived = props.row.state === "archived";
  const copy = archived ? forgetArchivedCopy(props.row) : forgetCopy(props.row, props.bound.binding);
  return (
    <>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="action-dlg-title">{copy.title}</h2></div>
      <div class="dlg-body">
        <div class="confirm-body" id="action-dlg-body">{copy.text}</div>
        <div class="dlg-detail">{copy.detail}</div>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn solid-danger" onClick={() => { props.close(); void (archived ? forgetArchived(props.row) : forget(props.row, props.bound)); }}>{copy.confirm}</button>
        <button type="button" class="btn primary" data-initial autofocus onClick={props.close}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/** Resume and open terminal's one question (Qt's confirmLocalStart before `hgs a`, SessionsWindow.cpp:5469-5472). */
function ResumeTerminalDialog(props: { row: ActionRow; answer: (confirmed: boolean) => void; close: () => void }) {
  onCleanup(() => props.answer(false));
  const confirm = () => { props.answer(true); props.close(); };
  return (
    <>
      <div class="dlg-head"><Icon name="square-terminal" size={18} class="accent" /><h2 id="action-dlg-title">{tr("Resume and open terminal?")}</h2></div>
      <div class="dlg-body"><div class="confirm-body" id="action-dlg-body" style={{ "white-space": "pre-line" }}>
        {tr("Resume %1 on %2, then open its terminal.\n\nThe agent continues its saved conversation. The terminal opens once this same session reports running.", props.row.name, props.row.machine)}
      </div></div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn" onClick={confirm}>{tr("Resume and open terminal")}</button>
        <button type="button" class="btn primary" data-initial autofocus onClick={props.close}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

function FreshDialog(props: { row: ActionRow; close: () => void }) {
  const copy = freshCopy(props.row);
  const confirm = () => {
    props.close();
    const live = catalog.rows[props.row.key];
    if (!live || !freshStillValid(snapshotOf(live), props.row, !computerOffline(live.computer), updating(live.computer, live.name), running())) {
      notify("error", tr(FRESH_CHANGED), true);
      return;
    }
    notify("info", tr("Starting %1 with a new conversation…", props.row.name));
    void perform(snapshotOf(live), { kind: "start_fresh" });
  };
  return (
    <>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="action-dlg-title">{copy.title}</h2></div>
      <div class="dlg-body"><div class="confirm-body" id="action-dlg-body" style={{ "white-space": "pre-line" }}>{copy.text}</div></div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn solid-danger" onClick={confirm}>{copy.confirm}</button>
        <button type="button" class="btn primary" data-initial autofocus onClick={props.close}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/**
 * A selection's Forget or Terminate (Qt's confirmSelectionAction): the text,
 * the targets behind a disclosure as Qt's detailed text, Cancel focused and
 * the default. Closing it any other way (Escape, another dialog) answers no.
 */
function BatchDialog(props: { copy: BatchConfirmation; answer: (confirmed: boolean) => void; close: () => void }) {
  onCleanup(() => props.answer(false));
  return (
    <>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="action-dlg-title">{props.copy.title}</h2></div>
      <div class="dlg-body">
        <div class="confirm-body" id="action-dlg-body" style={{ "white-space": "pre-line" }}>{props.copy.text}</div>
        <details class="dlg-detail">
          <summary>{tr("Show details")}</summary>
          <div style={{ "white-space": "pre-line", "max-height": "160px", overflow: "auto" }}>{props.copy.details.join("\n")}</div>
        </details>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn solid-danger" onClick={() => { props.answer(true); props.close(); }}>{props.copy.confirm}</button>
        <button type="button" class="btn primary" data-initial autofocus onClick={props.close}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/**
 * One modal host for the dialogs. In one code path it opens the modal,
 * then focuses the dialog's initial control (the tag field with its text
 * selected, or Cancel). Escape closes through the dialog's own cancel.
 */
export function ActionDialogs() {
  let el: HTMLDialogElement | undefined;
  createEffect(() => {
    const state = open();
    if (!el) return;
    if (!state) { if (el.open) el.close(); return; }
    if (!el.open) el.showModal();
    queueMicrotask(() => {
      const initial = el?.querySelector<HTMLElement>("[data-initial]");
      initial?.focus();
      if ((state.kind === "rename" || state.kind === "fork") && initial instanceof HTMLInputElement) initial.select();
    });
  });
  return (
    <dialog class="dlg" ref={el} role={open()?.kind === "rename" || open()?.kind === "fork" || !open() ? "dialog" : "alertdialog"} aria-labelledby="action-dlg-title"
      aria-describedby={open() && open()?.kind !== "rename" && open()?.kind !== "fork" ? "action-dlg-body" : undefined} style={{ width: open()?.kind === "fork" ? "480px" : "460px" }} onClose={closeDialog}>
      <Show when={open()} keyed>
        {(p) => (p.kind === "fork" ? <ForkDialog row={p.row} details={p.details} close={closeDialog} /> : p.kind === "rename" ? <RenameDialog row={p.row} close={closeDialog} />
          : p.kind === "fresh" ? <FreshDialog row={p.row} close={closeDialog} /> : p.kind === "forget" ? <ForgetDialog row={p.row} bound={p.bound} close={closeDialog} />
          : p.kind === "batch" ? <BatchDialog copy={p.copy} answer={p.answer} close={closeDialog} /> : p.kind === "terminate" ? <TerminateDialog row={p.row} close={closeDialog} />
          : p.kind === "archive" ? <ArchiveDialog row={p.row} close={closeDialog} />
          : p.kind === "resume_terminal" ? <ResumeTerminalDialog row={p.row} answer={p.answer} close={closeDialog} /> : null)}
      </Show>
    </dialog>
  );
}
