
import { trNoop, tr } from "../../../i18n";// The approval journal's reads as AutomaticApprovalSettings.cpp runs them:
// one list read at a time (an edit during a read reads once more, never a
// queue of pages), Newer and Older by the journal's cursors, nothing to page
// or select after a failed read, and an entry's detail where only the
// latest selection's answer counts.

export interface Entry { id: number; at: number; host: string; session: string; prompt: string; result: string }
/** Read identity only; native code rechecks it against the selected service. */
export interface JournalScope { configuration: string; epoch: string; generation: number; journal: string; through: number; host: string; session: string }
export interface Page { entries: Entry[]; next: number; previous: number; scope?: JournalScope }
export interface ListRequest { host: string; session: string; before: number; after: number; scope?: JournalScope }

export interface JournalState {
  page: Page;
  /** A read runs: nothing to page or select. */
  loading: boolean;
  status: string;
  selected?: string;
  detail?: string;
  detailHint: string;
}

export interface JournalIo {
  list(request: ListRequest): Promise<Page>;
  detail(id: number, scope?: JournalScope): Promise<string>;
}

export const PLACEHOLDER = trNoop("Select a journal entry to read the question and its outcome.");
const EMPTY: Page = { entries: [], next: 0, previous: 0 };

export function createJournal(io: JournalIo, filters: () => { host: string; session: string }, show: (state: JournalState) => void) {
  let state: JournalState = { page: EMPTY, loading: false, status: "", detailHint: tr(PLACEHOLDER) };
  let before = 0;
  let after = 0;
  let scope: JournalScope | undefined;
  let busy = false;
  let again = false;
  let detailRequest = 0;
  const update = (change: Partial<JournalState>) => { state = { ...state, ...change }; show(state); };

  const refresh = () => {
    detailRequest++;
    update({ status: tr("Loading approval journal…"), loading: true, selected: undefined, detail: undefined, detailHint: tr(PLACEHOLDER) });
    if (busy) { again = true; return; }
    busy = true;
    const done = (finish: () => void) => {
      busy = false;
      if (again) { again = false; refresh(); return; }
      finish();
    };
    io.list({ ...filters(), before, after, ...(scope ? { scope } : {}) }).then(
      (page) => done(() => update({
        page,
        loading: false,
        status: page.entries.length
          ? `${page.entries.length} entries. Unconfirmed attempts require checking Activity or Terminal; they are not retried.`
          : tr("No automatic approvals recorded for this filter."),
      })),
      // A failed read leaves no page to select from or move through.
      (error) => done(() => update({ page: EMPTY, loading: false, status: tr(String(error)) })),
    );
  };

  return {
    state: () => state,
    firstPage() { before = 0; after = 0; scope = undefined; refresh(); },
    newer() { if (state.loading || state.page.previous <= 0) return; after = state.page.previous; before = 0; scope = state.page.scope; refresh(); },
    older() { if (state.loading || state.page.next <= 0) return; before = state.page.next; after = 0; scope = state.page.scope; refresh(); },
    select(key: string) {
      if (state.loading) return;
      const request = ++detailRequest;
      update({ selected: key, detail: undefined, detailHint: tr("Loading question…") });
      io.detail(Number(key), state.page.scope).then(
        (text) => { if (request === detailRequest) update({ detail: text }); },
        (error) => { if (request === detailRequest) update({ detail: tr(String(error)) }); },
      );
    },
  };
}
