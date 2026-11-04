import { For, Show, createEffect, createRoot, createSignal, untrack, type JSX } from "solid-js";
import * as bridge from "../../bridge";
import { Icon } from "../lib/icons";
import { composer, draftKey } from "../../store/composer";
import { orderedRows } from "../../store/catalog";
import { openSession } from "../../store/desk";
import { setSubagentOpen, toast } from "../../store/ui";
import { AgentBadge, MachineBadge } from "./SessionCard";
import { listedSessionIgnoringCollapse } from "./model";
import {
  createSavedDrafts, draftOwnerRow, draftPresentation, draftSubagent, focusMessageEditor, listStep, modifiedText, REASONS, replaceText, SECTIONS, TEXTS,
  type RestoreTarget, type SavedDrafts,
} from "../../store/savedDrafts";
import type { SavedDraftItem } from "../../types";
import type { Session } from "./types";
import { tr, trNoop } from "../../i18n";

// The Saved drafts dialog (MessageComposer.cpp:1230-1387), opened from the
// session list's Saved drafts button (SessionsWindow.cpp:485-489). The list
// is one listbox of options grouped by section; its keys move the selection
// and Enter goes to the session (a double click's equivalent). Restore over a
// draft that is not empty asks "Replace current draft?" with Cancel as the
// default; Close is the dialog's default button, as in Qt. A button that is
// off stays focusable and names its reason as its tooltip.

/** The dialog's store; its rows go when it closes. */
export const savedDrafts: SavedDrafts = createRoot(() => createSavedDrafts({
  page: bridge.savedDrafts, conflicts: bridge.draftConflicts, detail: bridge.savedDraft,
  recover: bridge.draftRecover, restore: bridge.draftRestore,
}));

/** The element the focus returns to when the dialog closes (the list's Saved drafts button). */
let opener: HTMLElement | undefined;

/** Opens Saved drafts for the open composer `handle`; the focus returns to `from` when it closes. */
export function openSavedDrafts(from: HTMLElement | undefined, handle: number | undefined) {
  opener = from;
  savedDrafts.show(handle);
}

/** "Show more" per section, below the list (a listbox holds options only). */
export const MORE_LABEL: Record<(typeof SECTIONS)[number]["id"], string> = {
  client: trNoop("Show more on this computer"),
  qt: trNoop("Show more from the previous Zerus app"),
  conflicts: trNoop("Show more not imported at the switch"),
};

/** A row's element id, for the list's active descendant. */
const rowIds = new WeakMap<SavedDraftItem, string>();
let rowCount = 0;
const rowId = (item: SavedDraftItem) => {
  let id = rowIds.get(item);
  if (!id) rowIds.set(item, (id = `saved-draft-${++rowCount}`));
  return id;
};

/** The open composer as Restore sees it. */
function restoreTarget(): RestoreTarget | undefined {
  const v = composer.view();
  if (!v) return undefined;
  return {
    handle: v.handle, key: v.key,
    hasDraft: composer.text() !== "" || v.attachments.length > 0,
    idle: v.loaded && !v.sending && !v.waiting && !composer.operationLocked(),
  };
}

/** A footer button that stays focusable while off and names why as its tooltip. */
/** A reason from the store's tables reads translated; a detail line from Rust stays as it came. */
const KNOWN_REASONS = new Set<string>([...Object.values(REASONS), TEXTS.unreachable]);
const reasonText = (reason: string) => (KNOWN_REASONS.has(reason) ? tr(reason) : reason);

function Action(props: { reason: string; onClick: () => void; ref?: (el: HTMLButtonElement) => void; children: JSX.Element }) {
  return (
    <button type="button" class="btn" ref={props.ref} aria-disabled={props.reason ? "true" : undefined} title={props.reason ? reasonText(props.reason) : undefined}
      onClick={() => { if (!props.reason) props.onClick(); }}>{props.children}</button>
  );
}

function Row(props: { item: SavedDraftItem; onOpen: () => void }) {
  const on = () => savedDrafts.selected() === props.item;
  const presentation = () => draftPresentation(props.item, draftOwnerRow(props.item, orderedRows()));
  return (
    <div class="draft-row" classList={{ on: on() }} role="option" aria-selected={on()} aria-label={presentation().title}
      id={rowId(props.item)}
      onClick={() => { if (!on()) savedDrafts.select(props.item); }} onDblClick={() => props.onOpen()}>
      <div class="draft-row-head">
        <b class="ell">{presentation().title}</b>
        <Show when={presentation().agent}>{(agent) => <AgentBadge agent={agent()} narrow />}</Show>
        <span class="grow" />
        <Show when={presentation().computer !== undefined} fallback={<span class="draft-machine ell">{presentation().machine}</span>}>
          <MachineBadge alias={presentation().machine} computer={presentation().computer!} />
        </Show>
      </div>
      <Show when={presentation().checkout}><div class="draft-row-meta ell">{presentation().checkout}</div></Show>
      <div class="draft-row-foot">
        <span class="ell">{props.item.detail}</span>
        <Show when={presentation().model}><span class="draft-model ell">{presentation().model}</span></Show>
        <span class="grow" />
        <Show when={props.item.modified > 0}><span class="act-time">{modifiedText(props.item.modified)}</span></Show>
      </div>
    </div>
  );
}

/** "Replace current draft?": Cancel has the focus and is the default; Escape is Cancel. */
function ReplaceDialog(props: { open: boolean; text: string; onCancel: () => void; onConfirm: () => void }) {
  let el: HTMLDialogElement | undefined;
  createEffect(() => {
    if (!el) return;
    if (!props.open) { if (el.open) el.close(); return; }
    if (!el.open) el.showModal();
    queueMicrotask(() => el?.querySelector<HTMLElement>("[data-initial]")?.focus());
  });
  return (
    <dialog class="dlg" ref={el} role="alertdialog" aria-labelledby="replace-draft-title" aria-describedby="replace-draft-body" style={{ width: "460px" }}
      onCancel={(e) => { e.preventDefault(); props.onCancel(); }} onClose={() => { if (!el?.open && props.open) props.onCancel(); }}>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="replace-draft-title">{tr(TEXTS.replaceTitle)}</h2></div>
      <div class="dlg-body"><div class="confirm-body" id="replace-draft-body" style={{ "white-space": "pre-wrap" }}>{props.text}</div></div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn solid-danger" onClick={props.onConfirm}>{tr(TEXTS.replaceConfirm)}</button>
        <button type="button" class="btn primary" data-initial onClick={props.onCancel}>{tr("Cancel")}</button>
      </div>
    </dialog>
  );
}

export function SavedDraftsDialog() {
  let el: HTMLDialogElement | undefined;
  let list: HTMLDivElement | undefined;
  let restoreButton: HTMLButtonElement | undefined;
  const [replacing, setReplacing] = createSignal<string>();
  /** The list takes the focus once its first draft is selected after opening. */
  let focusList = false;
  /** The selected draft's session row, found when it is selected (Qt's canOpenDraftSession). */
  const [owner, setOwner] = createSignal<Session>();
  createEffect(() => {
    const item = savedDrafts.selected();
    setOwner(item ? untrack(() => draftOwnerRow(item, orderedRows())) : undefined);
    if (!item) return;
    queueMicrotask(() => {
      // The selected row stays in view.
      list?.querySelector(".draft-row.on")?.scrollIntoView({ block: "nearest" });
      if (focusList && list && el?.open) { focusList = false; list.focus(); }
    });
  });
  createEffect(() => {
    if (!el) return;
    if (!savedDrafts.open()) {
      setReplacing(undefined);
      if (el.open) el.close();
      return;
    }
    focusList = true;
    if (!el.open) el.showModal();
    queueMicrotask(() => el?.querySelector<HTMLElement>("[data-initial]")?.focus());
  });

  const all = () => SECTIONS.flatMap(({ id }) => savedDrafts.sections[id].items);
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Enter") { e.preventDefault(); goTo(); return; }
    const items = all();
    const row = list?.querySelector<HTMLElement>(".draft-row");
    const page = list && row ? Math.max(1, Math.floor(list.clientHeight / Math.max(1, row.offsetHeight)) - 1) : 1;
    const next = listStep(items.length, items.indexOf(savedDrafts.selected()!), e.key, page);
    if (next === undefined) return;
    e.preventDefault();
    if (items[next] !== savedDrafts.selected()) savedDrafts.select(items[next]);
  };

  const close = () => savedDrafts.close();
  const goToReason = () => (!savedDrafts.selected() ? trNoop("Select a draft.") : owner() ? "" : TEXTS.unreachable);
  const goTo = () => {
    const item = savedDrafts.selected();
    // The row is looked for again: the list may have changed since the selection.
    const row = item && draftOwnerRow(item, orderedRows());
    if (!row) return;
    opener = undefined;
    savedDrafts.close();
    openSession(
      { computer: row.computer, key: row.catalogKey, preserveFilters: true },
      { keepFiltersIf: listedSessionIgnoringCollapse },
    );
    if (item.kind === "subagent") {
      const child = draftSubagent(item, row);
      if (child) setSubagentOpen(child);
      return;
    }
    if (item.kind === "message") {
      const key = draftKey({ computer: row.computer, key: row.catalogKey, name: row.name, archive_id: row.archiveId ?? null });
      void focusMessageEditor(key);
    }
  };
  const restore = async (confirmed = false) => {
    const target = restoreTarget();
    if (!savedDrafts.canRestore(target)) return;
    if (!confirmed && savedDrafts.asks(target!)) { setReplacing(target!.key); return; }
    // What was typed reaches Rust first; Restore replaces it with a new text epoch.
    await composer.flush();
    if (await savedDrafts.restore(restoreTarget())) savedDrafts.close();
    else restoreButton?.focus();
  };
  const copy = () => {
    const text = savedDrafts.detail()?.text ?? "";
    if (!text) return;
    void bridge.copyVerbatim(text).then(() => toast(tr("Copied"), tr("The draft's text is on the clipboard")), () => toast(tr("Not copied"), tr("The clipboard is unavailable"), "error"));
  };

  const detailLine = () => {
    if (!savedDrafts.selected()) return "";
    return savedDrafts.detail()?.detail ?? tr(TEXTS.loading);
  };
  return (
    <dialog class="dlg saved-drafts" classList={{ "no-drafts": savedDrafts.empty() }} ref={el} aria-labelledby="saved-drafts-title" style={{ width: savedDrafts.empty() ? "480px" : "720px" }}
      onCancel={(e) => { e.preventDefault(); close(); }}
      onClose={() => {
        // A delayed close from the previous opening must not consume this opener.
        if (el?.open) return;
        if (savedDrafts.open()) savedDrafts.close();
        // The focus returns to what opened the dialog.
        const back = opener;
        opener = undefined;
        if (back?.isConnected) back.focus();
      }}>
      <div class="dlg-head"><Icon name="drafts" size={18} class="accent" /><h2 id="saved-drafts-title">{tr(TEXTS.title)}</h2></div>
      <div class="dlg-body saved-drafts-body">
        <p class="hint" id="saved-drafts-hint">{tr(savedDrafts.empty() ? TEXTS.empty : TEXTS.hint)}</p>
        <Show when={!savedDrafts.empty()}>
          <div class="draft-list" ref={list} role="listbox" tabindex="0" aria-label={tr(TEXTS.title)} aria-describedby="saved-drafts-hint"
            aria-activedescendant={savedDrafts.selected() ? rowId(savedDrafts.selected()!) : undefined} onKeyDown={onKey}>
            <For each={SECTIONS}>{(section) => (
              <Show when={savedDrafts.sections[section.id].items.length > 0}>
                <div class="draft-section" role="group" aria-labelledby={`draft-section-${section.id}`}>
                  <div class="eyebrow draft-section-title" id={`draft-section-${section.id}`} role="presentation">{tr(section.title)}</div>
                  <Show when={section.note}>{(note) => <p class="hint draft-section-note" role="presentation">{tr(note())}</p>}</Show>
                  <For each={savedDrafts.sections[section.id].items}>{(item) => <Row item={item} onOpen={goTo} />}</For>
                </div>
              </Show>
            )}</For>
          </div>
          <div class="draft-more-bar">
            <For each={SECTIONS}>{(section) => (
              <Show when={savedDrafts.sections[section.id].next !== null}>
                <button type="button" class="btn small" aria-disabled={savedDrafts.sections[section.id].loading ? "true" : undefined}
                  onClick={() => { void savedDrafts.more(section.id); }}>
                  {savedDrafts.sections[section.id].loading ? tr("Loading…") : tr(MORE_LABEL[section.id])}
                </button>
              </Show>
            )}</For>
          </div>
          <pre class="draft-preview mono" aria-label={tr("Draft text")} tabindex="0">{savedDrafts.detail()?.text ?? ""}</pre>
          <p class="hint draft-detail" aria-live="polite">{detailLine()}</p>
        </Show>
        <Show when={savedDrafts.message()}>
          {(m) => <p class="draft-message" classList={{ error: m().error }} role={m().error ? "alert" : "status"}>{m().error ? m().text : tr(m().text)}</p>}
        </Show>
      </div>
      <div class="dlg-foot">
        <Action reason={savedDrafts.copyReason()} onClick={copy}>{tr("Copy text")}</Action>
        <Action reason={savedDrafts.restoreReason(restoreTarget())} ref={(b) => { restoreButton = b; }} onClick={() => { void restore(); }}>{tr("Restore in current session")}</Action>
        <Action reason={goToReason()} onClick={goTo}>{tr("Go to session")}</Action>
        <Show when={savedDrafts.selected()?.source === "qt"}>
          <Action reason={savedDrafts.recoverReason()} onClick={() => { void savedDrafts.recover(); }}>
            {savedDrafts.recovering() ? tr("Recovering…") : tr("Recover")}
          </Action>
        </Show>
        <span class="spacer" />
        <button type="button" class="btn primary" data-initial onClick={close}>{tr("Close")}</button>
      </div>
      <ReplaceDialog open={!!replacing()} text={replaceText(replacing() ?? "")}
        onCancel={() => { setReplacing(undefined); restoreButton?.focus(); }}
        onConfirm={() => { setReplacing(undefined); void restore(true); }} />
    </dialog>
  );
}
