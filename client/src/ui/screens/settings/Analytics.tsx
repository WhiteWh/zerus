import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { inTauri } from "../../../bridge";
import { Icon } from "../../lib/icons";
import { Actions, Check, Group, Lines, NumberField, Row, SectionHead, Select, Status, Table } from "./ui";
import { tr, trNoop } from "../../../i18n";
import {
  PROVIDERS, coverageText, defaultRange, detailText, displayRoute, measure, overviewText, pageText, voiceText,
  type Report, type Row as Bucket,
} from "./analytics-text";

// Settings > Private analytics (tray/src/AnalyticsSettings.cpp) as the
// approved mockup lays it out (design/webui/src/screens/settings/Analytics.tsx).
// The core's analytics page holds the reads, epochs and paging; every read
// and change runs and is parsed off the main thread, and only `hgs` changes
// collection, on this computer alone.

interface View {
  status: string; busy: boolean; mutating: boolean; can_refresh: boolean; can_export: boolean; can_change_local: boolean;
  peers: string[]; peer_limit: boolean; selection: string; from: string | null; through: string | null;
  local: { enabled: boolean; retention_days: number; epoch: string; reads: number } | null;
  report: Report | null; purge_pending: boolean; shorten_pending: number | null;
}
interface Rows { rows: Bucket[]; offset: number; total: number }

const AGE_MS = 60_000;
const key = (r: Bucket) => `${r.date}|${r.route}|${r.provider}|${r.producer}`;

export function AnalyticsSection() {
  const range = defaultRange(Date.now());
  const [view, setView] = createSignal<View | undefined>();
  const [selection, setSelection] = createSignal("");
  const [from, setFrom] = createSignal(range.from);
  const [through, setThrough] = createSignal(range.through);
  const [provider, setProvider] = createSignal("");
  const [offset, setOffset] = createSignal(0);
  const [rows, setRows] = createSignal<Rows | undefined>();
  const [selected, setSelected] = createSignal<string | undefined>();
  const [enabled, setEnabled] = createSignal(false);
  const [retention, setRetention] = createSignal(90);
  const [review, setReview] = createSignal<string | undefined>();
  // A call that failed outright; the status line says why until the next view.
  const [failure, setFailure] = createSignal<string | undefined>();
  let localReads = 0;

  const readRows = () => {
    invoke<Rows>("analytics_rows", { provider: provider(), offset: offset() }).then((page) => {
      setRows(page);
      if (page.offset !== offset()) setOffset(page.offset);
      // Qt clears the detail whenever it draws the rows.
      setSelected(undefined);
    }, (error) => setFailure(String(error)));
  };
  const failed = (error: unknown) => setFailure(String(error));
  const apply = (next: View) => {
    setFailure(undefined);
    setView(next);
    // The selected peer left: Qt's list falls back to this computer.
    if (selection() !== "" && selection() !== "*" && !next.peers.includes(selection())) setSelection("");
    // Each current read of this computer sets the controls, as in Qt.
    if (next.local && next.local.reads !== localReads) {
      localReads = next.local.reads;
      setEnabled(next.local.enabled);
      setRetention(next.local.retention_days);
    }
    readRows();
  };
  const fetchView = () => { void invoke<View>("analytics_view").then(apply, failed); };
  // The clicks that may open a question, still on their way.
  let changing: Promise<unknown> = Promise.resolve();
  const change = <T,>(call: Promise<T>) => { changing = Promise.all([changing, call.catch(() => {})]); return call; };
  const refresh = () => {
    if (!inTauri()) return;
    setOffset(0);
    void invoke<View>("analytics_refresh", { selection: selection(), from: from(), through: through() }).then(apply, failed);
  };

  onMount(() => {
    if (!inTauri()) return;
    let gone = false;
    let stop: (() => void) | undefined;
    // Once listening, the view is read again: an answer that came before
    // would otherwise wait for the next one.
    listen<{ refresh: boolean }>("analytics-changed", (e) => { if (e.payload.refresh) refresh(); else fetchView(); })
      .then((off) => { if (gone) off(); else { stop = off; fetchView(); } }, () => {});
    // The page starts from the last read's computers and days, as Qt's
    // controls keep them; a question left from an earlier visit is
    // cancelled; then the section reads, as Qt's does when opened.
    void invoke<View>("analytics_configure_answer", { confirm: false })
      .then(() => invoke<View>("analytics_purge", { confirm: false })).then((last) => {
      if (last.from && last.through) {
        setSelection(last.selection);
        setFrom(last.from);
        setThrough(last.through);
      }
      refresh();
    }, () => refresh());
    const timer = setInterval(() => {
      if (!view()?.busy) void invoke<View>("analytics_age").then(apply, failed);
    }, AGE_MS);
    onCleanup(() => {
      gone = true;
      stop?.();
      clearInterval(timer);
      // Whatever the last view showed, once the clicks still on their way
      // have answered: a question asked meanwhile is cancelled too, and a
      // cancel with none open does nothing.
      void changing.then(() => Promise.all([
        invoke("analytics_configure_answer", { confirm: false }),
        invoke("analytics_purge", { confirm: false }),
      ])).catch(() => {});
    });
  });
  createEffect(on([provider, offset], () => { if (inTauri() && view()?.report) readRows(); }, { defer: true }));

  const report = () => view()?.report ?? undefined;
  const detail = () => rows()?.rows.find((r) => key(r) === selected());
  const page = () => rows() ?? { rows: [], offset: 0, total: 0 };
  const computers = () => [
    { value: "", label: tr("This computer") },
    { value: "*", label: view()?.peer_limit ? "First 32 configured computers" : tr("All configured computers") },
    ...(view()?.peers ?? []).map((peer) => ({ value: peer, label: peer })),
  ];
  // Checked at the click (Qt's fresh(m_local)) and pinned to the epoch of
  // this computer's read; a shorter retention is asked first
  // (shorten_pending), and the status says why a click does nothing.
  const applyLocal = () => {
    void change(invoke<View>("analytics_configure", { enabled: enabled(), retentionDays: retention() })).then(apply, failed);
  };
  // A question's answer; one that failed asks again (the page still
  // holds its question).
  const answer = (command: string) => (yes: boolean) =>
    invoke<View>(command, { confirm: yes }).then(apply, (error) => { failed(error); throw error; });
  const openReview = () => {
    void invoke<string | null>("analytics_review").then((text) => { if (text) setReview(text); }, failed);
  };
  const working = () => !!view()?.busy || !!view()?.mutating;

  return (
    <>
      <SectionHead title={trNoop("Private analytics")} intro={tr("Content-free daily counters stay on each computer. Reads do not enable collection. Unknown measures are unavailable, not zero. Dates are inclusive UTC days (up to 90).")} />
      <Group>
        <Actions>
          <Select value={selection()} label={trNoop("Selected computers")} onChange={setSelection} options={computers()} />
          <label class="an-date"><span class="set-label">{tr("From")}</span>
            <input class="field tabular" type="date" aria-label={tr("From UTC day")} value={from()} onChange={(e) => setFrom(e.currentTarget.value)} />
          </label>
          <label class="an-date"><span class="set-label">{tr("Through")}</span>
            <input class="field tabular" type="date" aria-label={tr("Through UTC day")} value={through()} onChange={(e) => setThrough(e.currentTarget.value)} />
          </label>
          <button type="button" class="btn primary" disabled={!view()?.can_refresh} onClick={refresh}><Icon name="refresh-cw" size={13} />{tr("Apply and refresh")}</button>
        </Actions>
        <Show when={failure() ?? view()?.status}>{(text) => <Status kind={failure() ? "error" : working() ? "busy" : "muted"}>{text()}</Status>}</Show>
        <Lines class="an-overview" text={report() ? overviewText(report()!) : tr("Choose a range and refresh to read analytics.")} />
        <Show when={report()}>
          {(r) => (
            <div class="an-coverage">
              <Show when={r().peer_limit}><Lines class="an-cov" text={tr("This view includes at most 32 configured computers. Additional configured routes are omitted.")} /></Show>
              <For each={r().missing}>{(m) => <Lines class="an-cov stale" text={`${displayRoute(m.route)}: ${m.error}`} />}</For>
              <For each={r().computers}>{(c) => <Lines class={c.current ? "an-cov" : "an-cov stale"} text={coverageText(c)} />}</For>
            </div>
          )}
        </Show>
      </Group>

      <Group title={trNoop("Daily agent work")} aside={
        <Select value={provider()} label={trNoop("Provider")} onChange={(v) => { setProvider(v); setOffset(0); }}
          options={[{ value: "", label: tr("All providers") }, ...PROVIDERS.map((p) => ({ value: p, label: p }))]} />
      }>
        <div class="an-split">
          <Table class="an-table" label={trNoop("Daily agent work")} rows={page().rows} rowKey={key} selected={selected()} onSelect={setSelected}
            empty={<span class="muted">{view()?.busy ? tr("Reading…") : tr("No current read available.")}</span>}
            columns={[
              { key: "day", label: trNoop("UTC day"), class: "tabular", render: (r) => r.date },
              { key: "computer", label: trNoop("Computer"), render: (r) => displayRoute(r.route) + (r.current ? "" : tr(" (last known)")) },
              { key: "provider", label: trNoop("Provider"), render: (r) => r.provider },
              { key: "producer", label: trNoop("Producer"), render: (r) => r.producer },
              { key: "turns", label: trNoop("Completed turns"), class: "num", render: (r) => measure(r.counters, "turn_completed") },
              { key: "work", label: trNoop("Work"), class: "num", render: (r) => measure(r.counters, "work_ms") },
              { key: "approval", label: trNoop("Approval wait"), class: "num", render: (r) => measure(r.counters, "approval_wait_ms") },
              { key: "input", label: trNoop("Input wait"), class: "num", render: (r) => measure(r.counters, "input_wait_ms") },
            ]} />
          <aside class="an-detail">
            <Show when={detail()} fallback={<p class="muted">{tr("Select a day to see all reported counts, token usage, typed failures and histogram means.")}</p>}>
              {(r) => <Lines text={detailText(r())} />}
            </Show>
          </aside>
        </div>
        <Actions>
          <button type="button" class="btn small" disabled={page().offset === 0} onClick={() => setOffset(Math.max(0, page().offset - 50))}><Icon name="chevron-left" size={12} />{tr("Previous")}</button>
          <button type="button" class="btn small" disabled={page().offset + page().rows.length >= page().total} onClick={() => setOffset(page().offset + 50)}>{tr("Next")}<Icon name="chevron-right" size={12} /></button>
          <span class="muted tabular">{pageText(page().offset, page().rows.length, page().total)}</span>
        </Actions>
      </Group>

      <Group title={trNoop("Voice on processing computers")} hint={trNoop("Voice counts are separate from agent turns. Accepted audio and total queue/processing time have units; percentile latency is unavailable. Speech collection is managed independently on its processing host.")}>
        <div class="an-voice">
          <For each={report()?.voice ?? []}>{(v) => <Lines class="muted" text={voiceText(v)} />}</For>
        </div>
      </Group>

      <Group title={trNoop("Collection on this computer")} hint={trNoop("Disabling collection keeps existing history. Purge removes only this computer's agent analytics, not speech statistics or durable delivery receipts. Remote collection controls are read-only here.")}>
        <Check label={trNoop("Collect content-free agent analytics")} checked={enabled()} disabled={!view()?.can_change_local} onChange={setEnabled} />
        <Row label={trNoop("Daily aggregate retention")}>
          <NumberField value={retention()} min={1} max={90} suffix="days" label={trNoop("Daily aggregate retention")}
            disabled={!view()?.can_change_local} onChange={setRetention} />
        </Row>
        <Actions>
          <button type="button" class="btn primary" disabled={!view()?.can_change_local} onClick={applyLocal}>{tr("Apply locally")}</button>
          <button type="button" class="btn danger" disabled={!view()?.can_change_local}
            onClick={() => void change(invoke<View>("analytics_purge_check")).then(apply, failed)}>
            <Icon name="trash" size={13} />{tr("Purge local history…")}
          </button>
        </Actions>
      </Group>
      <Actions>
        <button type="button" class="btn" disabled={!view()?.can_export} onClick={openReview}><Icon name="file-text" size={14} />{tr("Review aggregate diagnostics…")}</button>
      </Actions>

      <Show when={view()?.shorten_pending}>
        {(days) => (
          <Question title={tr("Shorten local analytics history?")} yes={tr("Shorten retention")}
            text={tr("Keep only the latest %1 UTC days of agent analytics on this computer. Older daily aggregates will be deleted. Speech statistics and durable delivery receipts are unaffected.", days())}
            answer={answer("analytics_configure_answer")} />
        )}
      </Show>
      <Show when={view()?.purge_pending}>
        <Question title={tr("Purge local agent analytics?")} yes={tr("Purge local history")}
          text={tr("Permanently delete all retained agent analytics on this computer. Speech statistics, other computers and durable delivery receipts are unaffected. Collection keeps its current enabled/disabled setting.")}
          answer={answer("analytics_purge")} />
      </Show>
      <Show when={review()}>
        {(text) => <Review text={text()} close={(save) => {
          setReview(undefined);
          // Cancel keeps nothing to save; only Save file… writes, after its own dialog.
          void invoke(save ? "analytics_save_review" : "analytics_review_cancel").catch(failed);
        }} />}
      </Show>
    </>
  );
}

/** A destructive question: the action, or Cancel (the default). An answer
 *  that could not be delivered asks again. */
function Question(props: { title: string; text: string; yes: string; answer: (yes: boolean) => Promise<unknown> }) {
  let dialog: HTMLDialogElement | undefined;
  const answer = (yes: boolean) => {
    if (dialog?.open) dialog.close();
    props.answer(yes).catch(() => { if (dialog?.isConnected && !dialog.open) dialog.showModal(); });
  };
  onCleanup(() => { if (dialog?.open) dialog.close(); });
  return (
    <dialog class="dlg" ref={(el) => { dialog = el; queueMicrotask(() => el.showModal()); }} aria-labelledby="analytics-question-title"
      onCancel={(e) => { e.preventDefault(); answer(false); }}>
      <div class="dlg-head"><h2 id="analytics-question-title">{props.title}</h2></div>
      <div class="dlg-body">{props.text}</div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn danger" onClick={() => answer(true)}>{props.yes}</button>
        <button type="button" class="btn primary" autofocus onClick={() => answer(false)}>{tr("Cancel")}</button>
      </div>
    </dialog>
  );
}

/** Review aggregate diagnostics: the exact text Save file… saves. */
function Review(props: { text: string; close: (save: boolean) => void }) {
  let dialog: HTMLDialogElement | undefined;
  const close = (save: boolean) => { if (dialog?.open) dialog.close(); props.close(save); };
  onCleanup(() => { if (dialog?.open) dialog.close(); });
  return (
    <dialog class="dlg wide" ref={(el) => { dialog = el; queueMicrotask(() => el.showModal()); }} aria-labelledby="analytics-review-title"
      onCancel={(e) => { e.preventDefault(); close(false); }}>
      <div class="dlg-head"><h2 id="analytics-review-title">{tr("Review aggregate diagnostics")}</h2></div>
      <div class="dlg-body">
        {tr("Review these aggregate fields before saving a local JSON file. Computer numbers are anonymous within this file. This export includes no logs, message content, account details or native identifiers, and is never uploaded automatically.")}
        <pre class="an-json" aria-label={tr("Aggregate diagnostics")}>{props.text}</pre>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn" onClick={() => close(true)}>{tr("Save file…")}</button>
        <button type="button" class="btn primary" autofocus onClick={() => close(false)}>{tr("Cancel")}</button>
      </div>
    </dialog>
  );
}
