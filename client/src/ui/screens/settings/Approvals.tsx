import { desktopService, serviceApprovalIntent } from "../../../store/desktopService";
import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../../../bridge";
import { flag, text, setSetting } from "../../../store/settings";
import { bus } from "../../../store/notices";
import ApprovalsConfirm from "./ApprovalsConfirm";
import { APPROVALS_KEY, APPROVALS_WARNING, APPROVALS_HINT, createApprovalOptIn } from "./approval-opt-in";
import { Icon } from "../../lib/icons";
import { Actions, Group, SectionHead, Status, Table, Switch } from "./ui";
import { PLACEHOLDER, createJournal, type JournalState, type Page } from "./approvals-journal";
import { tr, trNoop } from "../../../i18n";
import { canToggleServiceApproval } from "./approval-readiness";

// Settings > Automatic approvals (tray/src/AutomaticApprovalSettings.cpp) as
// the approved mockup lays it out. The separate opt-in is never seeded from
// Qt. The native worker owns delivery; this page reads 50 journal entries at a time.

/** QLocale().toString(QDateTime, QLocale::ShortFormat): the person's local time. */
export const entryTime = (at: number) => new Date(at).toLocaleString(undefined, { dateStyle: "short", timeStyle: "short" });

export function ApprovalsSection() {
  const optIn = createApprovalOptIn((key, enabled) => desktopService.enabled() ? serviceApprovalIntent.submit(enabled) : setSetting(key, enabled), (error) => bus.notify("error", String(error), true));
  const [host, setHost] = createSignal("");
  const [session, setSession] = createSignal("");
  const [state, setState] = createSignal<JournalState>({ page: { entries: [], next: 0, previous: 0 }, loading: false, status: "", detailHint: tr(PLACEHOLDER) });
  const journal = createJournal({
    list: (request) => invoke<Page>("approvals_list", { ...request }),
    detail: (id, scope) => invoke<string>("approvals_detail", { id, scope }),
  }, () => ({ host: host(), session: session() }), setState);
  // Outside the app there is no journal to read.
  const firstPage = () => { if (inTauri()) journal.firstPage(); };
  let debounce: ReturnType<typeof setTimeout> | undefined;
  const edited = () => { clearTimeout(debounce); debounce = setTimeout(firstPage, 300); };
  onMount(() => {
    firstPage();
    // Reads native pending/committed identity; it cannot submit another intent.
    if (inTauri() && desktopService.enabled()) void serviceApprovalIntent.check().catch(() => {});
  });
  onCleanup(() => clearTimeout(debounce));
  const page = () => state().page;
  const loading = () => state().loading;

  return (
    <>
      <SectionHead title={trNoop("Automatic approvals")} />
      <Group class="appr-toggle">
        <div class="set-row"><div class="set-row-text"><label class="set-label danger" for="appr-on">{tr("Automatically approve tool requests (dangerous)")}</label></div>
          <div class="set-row-control"><Switch id="appr-on" checked={desktopService.enabled() ? desktopService.state()?.automatic_approvals === true : flag(APPROVALS_KEY)} label={trNoop("Automatically approve tool requests (dangerous)")} disabled={optIn.busy() || (desktopService.enabled() && (!canToggleServiceApproval(desktopService.state()) || !!serviceApprovalIntent.pending()))}
            onChange={(on) => optIn.request(on, document.getElementById("appr-on") ?? document.activeElement as HTMLElement)} /></div></div>
        <Show when={desktopService.enabled()}><p class="set-group-hint">{tr("Automatic approvals are owned by the background service and may continue while this window is closed.")}</p></Show>
        <Show when={desktopService.enabled() && serviceApprovalIntent.pending()}>
          <Actions>
            <span class="muted">{tr("The previous approval setting is unconfirmed.")}</span>
            <button type="button" class="btn small" disabled={optIn.busy() || serviceApprovalIntent.checking()}
              onClick={() => void serviceApprovalIntent.check().catch(error => bus.notify("error", String(error), true))}>{tr("Check result")}</button>
          </Actions>
        </Show>
        <div class="notice danger"><Icon name="triangle-alert" size={14} /><span>{tr(APPROVALS_WARNING)}</span></div>
        <p class="set-group-hint">{APPROVALS_HINT}</p>
      </Group>
      <Show when={optIn.origin()} keyed>{(origin) => <ApprovalsConfirm origin={origin} actions={optIn} />}</Show>

      <Group title={trNoop("Approval journal")}>
        <Actions>
          <input class="field grow" placeholder={tr("Exact computer name (optional)")} aria-label={tr("Exact computer name (optional)")}
            value={host()} onInput={(e) => { setHost(e.currentTarget.value); edited(); }} />
          <input class="field grow" placeholder={tr("Exact session name (optional)")} aria-label={tr("Exact session name (optional)")}
            value={session()} onInput={(e) => { setSession(e.currentTarget.value); edited(); }} />
          <button type="button" class="btn" onClick={firstPage}><Icon name="refresh-cw" size={13} />{tr("Refresh")}</button>
        </Actions>
        <div class="an-split">
          <Table class="an-table" label={trNoop("Approval journal")} rows={loading() ? [] : page().entries} rowKey={(e) => String(e.id)}
            selected={state().selected} onSelect={journal.select}
            empty={<span class="muted">{loading() ? tr("Loading approval journal…") : tr("No automatic approvals recorded for this filter.")}</span>}
            columns={[
              { key: "time", label: "Time", class: "tabular", render: (e) => <span title={entryTime(e.at)}>{entryTime(e.at)}</span> },
              { key: "computer", label: "Computer", render: (e) => <span title={e.host}>{e.host}</span> },
              { key: "session", label: "Session", render: (e) => <span title={e.session}>{e.session}</span> },
              { key: "question", label: "Question", class: "ell-cell", render: (e) => <span title={e.prompt}>{e.prompt}</span> },
              { key: "result", label: "Result", render: (e) => <span title={e.result}>{e.result}</span> },
            ]} />
          <aside class="an-detail appr-detail">
            <Show when={state().detail} fallback={<p class="muted">{state().detailHint}</p>}>{(text) => <div class="appr-text">{text()}</div>}</Show>
          </aside>
        </div>
        <Actions>
          <button type="button" class="btn small" disabled={loading() || page().previous <= 0} onClick={journal.newer}>{tr("Newer")}</button>
          <button type="button" class="btn small" disabled={loading() || page().next <= 0} onClick={journal.older}>{tr("Older")}</button>
          <span class="muted">{tr("Up to 50 entries per page")}</span>
        </Actions>
        <Show when={text("automaticApprovals/error") || state().status}>{(value) => <Status kind={text("automaticApprovals/error") ? "error" : loading() ? "busy" : "muted"}>{value()}</Status>}</Show>
      </Group>
    </>
  );
}
