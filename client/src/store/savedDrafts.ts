import { dayTime } from "../ui/lib/usage";
import { batch, createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import type { ConflictsPage, DraftSource, SavedDraftDetail, SavedDraftItem, SavedDraftsPage } from "../types";
import type { Session } from "../ui/sessions/types";
import { folderOf } from "../ui/sessions/SessionCard";
import { composer } from "./composer";
import { tr, trNoop } from "../i18n";

// Saved drafts (Qt's MessageComposer::showSavedDrafts, MessageComposer.cpp
// 1230-1387) over src-tauri's composer/saved.rs. Three sections: this
// computer's drafts, Qt's ("From the previous Zerus app") and the copies the
// switch import left where they were. Each page is one bounded read of at
// most PAGE drafts; Show more reads the next on request only, and closing the
// dialog drops every row, so nothing loads a whole store or grows by itself.
// Qt's store and the dev app's are only read.

/** Drafts per page (Rust clamps to 1-200). */
export const PAGE = 100;

export const TEXTS = {
  title: trNoop("Saved drafts"),
  hint: trNoop("Drafts stay on this desktop, including those from ended or unavailable sessions. Restoring never sends a message."),
  empty: trNoop("No saved drafts on this desktop."),
  loading: trNoop("Loading draft…"),
  unreachable: trNoop("This session is not in the session list now."),
  replaceTitle: trNoop("Replace current draft?"),
  replaceConfirm: trNoop("Replace draft"),
  recovered: trNoop("Recovered to this computer."),
} as const;

/**
 * "Replace the unsent draft in {key} with the selected saved draft?"
 * (MessageComposer.cpp:1365). Qt joins the key's lines with " / ", which
 * reads " / claude/..." for this computer's empty machine: the empty parts
 * are left out.
 */
export const replaceText = (key: string) => tr("Replace the unsent draft in %1 with the selected saved draft?", key.split("\n").filter(Boolean).join(" / "));

/** Why a button is off; each shows as its tooltip. */
export const REASONS = {
  select: trNoop("Select a draft."),
  loading: trNoop("Loading draft…"),
  noText: trNoop("This draft has no text."),
  noSession: trNoop("Select a session to restore a draft into it."),
  otherSession: trNoop("Another session opened after Saved drafts. Open Saved drafts again to restore into it."),
  /** src-tauri `saved::RESTORE_LATER`. */
  composerBusy: trNoop("The current draft cannot be replaced while it is loading, sending or waiting to send."),
  working: trNoop("Wait for the current action to finish."),
} as const;

/**
 * The row a key moves the selection to (ArrowUp and ArrowDown by one, Home
 * and End to the ends, PageUp and PageDown by `page` rows), or undefined for
 * another key or an empty list.
 */
export function listStep(count: number, index: number, key: string, page: number): number | undefined {
  if (count <= 0) return undefined;
  const last = count - 1;
  const from = Math.min(Math.max(index, -1), last);
  switch (key) {
    case "ArrowDown": return Math.min(last, from + 1);
    case "ArrowUp": return Math.max(0, from - 1);
    case "Home": return 0;
    case "End": return last;
    case "PageDown": return Math.min(last, Math.max(0, from) + Math.max(1, page));
    case "PageUp": return Math.max(0, from - Math.max(1, page));
  }
  return undefined;
}

export type SectionId = "client" | "qt" | "conflicts";
export const SECTIONS: { id: SectionId; title: string; note?: string }[] = [
  { id: "client", title: trNoop("On this computer") },
  { id: "qt", title: trNoop("From the previous Zerus app") },
  { id: "conflicts", title: trNoop("Not imported at the switch"), note: trNoop("A different draft was already here, so these copies stayed where they were.") },
];

interface Section {
  items: SavedDraftItem[];
  /** The next page's token (client, qt) or offset (conflicts); null when none is left. */
  next: string | number | null;
  loading: boolean;
  /** Its first page answered. */
  loaded: boolean;
}

export interface SavedDraftsPort {
  page(source: "client" | "qt", limit: number, before: string | null): Promise<SavedDraftsPage>;
  conflicts(limit: number, after: number): Promise<ConflictsPage>;
  detail(source: DraftSource, key: string): Promise<SavedDraftDetail>;
  recover(key: string): Promise<void>;
  restore(handle: number, source: DraftSource, key: string): Promise<void>;
}

/** What Restore needs of the open composer (store/composer.ts). */
export interface RestoreTarget {
  handle: number;
  key: string;
  /** The editor holds text or files (Qt's current draft is not empty). */
  hasDraft: boolean;
  /** Loaded and not sending, waiting, attaching or otherwise locked. */
  idle: boolean;
}

const errorText = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));
const emptySection = (): Section => ({ items: [], next: null, loading: false, loaded: false });

/**
 * The catalog row a draft's session is (Qt's SessionsWindow::draftOwner):
 * none for an unknown or archived owner. The row of the key's host alias
 * wins; a subagent key names its machine instead.
 */
export function draftOwnerRow(item: SavedDraftItem, rows: Iterable<Session>): Session | undefined {
  if (item.kind === "unknown" || item.archived || !item.session) return undefined;
  let named: Session | undefined;
  for (const s of rows) {
    if (s.name !== item.session || s.state === "archived") continue;
    if (s.computer === item.computer) return s;
    if (item.computer && s.rawMachine === item.computer) named = s;
  }
  return named;
}

/** A subagent to open only while it is in the draft's conversation (Qt's childKey check). */
export function draftSubagent(item: SavedDraftItem, owner: Session): string | undefined {
  if (item.kind !== "subagent") return undefined;
  const parts = item.key.split("\n");
  const conversation = parts[2] ?? "";
  const child = parts[3] ?? "";
  if (!child || owner.conversationId !== conversation || !owner.subagents?.some((agent) => agent.id === child)) return undefined;
  return child;
}

export async function focusMessageEditor(key: string): Promise<void> {
  const deadline = performance.now() + 2000;
  for (;;) {
    const view = composer.view();
    if (view?.loaded && view.key === key) {
      if (view.input_visible !== false) document.querySelector<HTMLTextAreaElement>(".cmp-editor")?.focus();
      return;
    }
    const left = deadline - performance.now();
    if (left <= 0) return;
    await new Promise((resolve) => setTimeout(resolve, Math.min(25, left)));
  }
}

/** The catalog roles a Saved drafts row takes when its owner is listed. */
export function draftPresentation(item: SavedDraftItem, owner: Session | undefined) {
  return {
    title: owner?.title ?? item.caption,
    machine: owner?.machine ?? item.machine,
    agent: owner?.agent,
    computer: owner?.computer,
    checkout: owner ? [folderOf(owner), owner.branch].filter(Boolean).join(" / ") : item.meta,
    model: owner && item.kind !== "subagent" ? [owner.model, owner.effort].filter(Boolean).join(" ") : "",
  };
}

/** "d MMM HH:mm" in local time; unknown timestamps stay empty. */
export const modifiedText = (seconds: number): string => seconds > 0 ? dayTime(seconds) : "";

export function createSavedDrafts(port: SavedDraftsPort) {
  const [open, setOpen] = createSignal(false);
  const [sections, setSections] = createStore<Record<SectionId, Section>>({ client: emptySection(), qt: emptySection(), conflicts: emptySection() });
  const [selected, setSelected] = createSignal<SavedDraftItem>();
  /** The selected draft's preview; undefined while it loads ("Loading draft…"). */
  const [detail, setDetail] = createSignal<SavedDraftDetail>();
  /** Why Restore or Recover did not run, or that Recover ran. */
  const [message, setMessage] = createSignal<{ text: string; error: boolean }>();
  /** A Restore or Recover on its way. */
  const [pending, setPending] = createSignal<"restore" | "recover">();
  const busy = () => pending() !== undefined;
  /** Counts opens and closes: an answer of an earlier opening is dropped. */
  let opening = 0;
  /** Counts selections (Qt's `selection`): a late preview of another draft is dropped. */
  let selection = 0;
  /** The composer the dialog opened for (Qt's `target`): Restore replaces only its draft. */
  let target: number | undefined;

  const key = (item: SavedDraftItem) => `${item.source}\n${item.key}`;

  function select(item: SavedDraftItem | undefined) {
    const mine = ++selection;
    const at = opening;
    batch(() => { setSelected(item); setDetail(undefined); setMessage(undefined); });
    if (!item) return;
    port.detail(item.source, item.key).then(
      (found) => { if (mine === selection && at === opening) setDetail(found); },
      (error) => { if (mine === selection && at === opening) setDetail({ text: "", attachments: 0, detail: errorText(error), restorable: false }); },
    );
  }

  /** The first draft listed, once nothing is selected (Qt selects row 0). */
  function selectFirst() {
    if (selected()) return;
    for (const { id } of SECTIONS) {
      const first = sections[id].items[0];
      if (first) { select(first); return; }
    }
  }

  async function load(id: SectionId) {
    const section = sections[id];
    if (section.loading || (section.loaded && section.next === null)) return;
    const at = opening;
    setSections(id, "loading", true);
    try {
      let items: SavedDraftItem[];
      let next: string | number | null;
      if (id === "conflicts") {
        const page = await port.conflicts(PAGE, typeof section.next === "number" ? section.next : 0);
        ({ items, next } = page);
      } else {
        const page = await port.page(id, PAGE, typeof section.next === "string" ? section.next : null);
        ({ items, next } = page);
      }
      if (at !== opening) return;
      setSections(id, produce((s) => {
        // A page never repeats a draft the section shows (a draft saved meanwhile moves to the front).
        const shown = new Set(s.items.map(key));
        s.items.push(...items.filter((item) => !shown.has(key(item))));
        s.next = next;
        s.loaded = true;
        s.loading = false;
      }));
      selectFirst();
    } catch {
      if (at === opening) setSections(id, produce((s) => { s.loading = false; s.loaded = true; s.next = null; }));
    }
  }

  function reset() {
    opening++;
    selection++;
    batch(() => {
      setSections({ client: emptySection(), qt: emptySection(), conflicts: emptySection() });
      setSelected(undefined);
      setDetail(undefined);
      setMessage(undefined);
      setPending(undefined);
    });
  }

  /** Why Restore in current session is off, "" when it is on (MessageComposer.cpp:1321-1323). */
  function restoreReason(composer: RestoreTarget | undefined): string {
    const d = detail();
    if (!selected()) return REASONS.select;
    if (!d) return REASONS.loading;
    // A storage error or a voice message: the detail line says which.
    if (!d.restorable) return d.detail;
    if (!composer || target === undefined) return REASONS.noSession;
    if (composer.handle !== target) return REASONS.otherSession;
    if (!composer.idle) return REASONS.composerBusy;
    if (busy()) return REASONS.working;
    return "";
  }

  return {
    open, sections, selected, detail, message, busy,
    /** Recover is on its way ("Recovering…"). */
    recovering: () => pending() === "recover",
    restoreReason,
    /** Why Copy text is off, "" when it is on. */
    copyReason(): string {
      if (!selected()) return REASONS.select;
      const d = detail();
      if (!d) return REASONS.loading;
      return d.text ? "" : REASONS.noText;
    },
    /** Why Recover is off, "" when it is on. */
    recoverReason: (): string => (busy() ? REASONS.working : ""),
    /** Every section's first page answered and none lists a draft. */
    empty: () => SECTIONS.every(({ id }) => sections[id].loaded && sections[id].items.length === 0),
    /** Opens for the composer `handle` (undefined: Restore stays off). */
    show(handle: number | undefined) {
      reset();
      target = handle;
      setOpen(true);
      for (const { id } of SECTIONS) void load(id);
    },
    /** Closes and drops every row. */
    close() {
      reset();
      target = undefined;
      setOpen(false);
    },
    /** Show more: the section's next page, on request only. */
    more: load,
    select,
    /** Restore in current session is enabled (MessageComposer.cpp:1321-1323). */
    canRestore: (composer: RestoreTarget | undefined): boolean => restoreReason(composer) === "",
    /**
     * Whether Restore asks "Replace current draft?" first: the open draft is
     * not empty and the selection is not that very draft (1362-1364).
     */
    asks(composer: RestoreTarget): boolean {
      const item = selected();
      return composer.hasDraft && !(item?.source === "client" && item.key === composer.key);
    },
    /** Restores the selection into the composer; true when the dialog may close. */
    async restore(composer: RestoreTarget | undefined): Promise<boolean> {
      const item = selected();
      if (!item || !composer || composer.handle !== target || !composer.idle || busy()) return false;
      setPending("restore");
      try {
        await port.restore(composer.handle, item.source, item.key);
        return true;
      } catch (error) {
        setMessage({ text: errorText(error), error: true });
        return false;
      } finally {
        setPending(undefined);
      }
    },
    /** Recover (Qt's drafts only): a copy in this computer's store, never over a draft here. */
    async recover() {
      const item = selected();
      if (!item || item.source !== "qt" || busy()) return;
      const at = opening;
      setPending("recover");
      setMessage(undefined);
      try {
        await port.recover(item.key);
        if (at !== opening) return;
        setMessage({ text: TEXTS.recovered, error: false });
        // The recovered draft now heads this computer's section.
        setSections("client", emptySection());
        void load("client");
      } catch (error) {
        if (at === opening) setMessage({ text: errorText(error), error: true });
      } finally {
        if (at === opening) setPending(undefined);
      }
    },
  };
}

export type SavedDrafts = ReturnType<typeof createSavedDrafts>;
