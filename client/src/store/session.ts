import { desktopService } from "./desktopService";
import { batch, createEffect, createRoot, createSignal, on, untrack } from "solid-js";
import { activityPort, composerTab, inspectSubagentActivity, processesFollow, processesShown, windowVisibility } from "../bridge";
import { applyBatch, catalog, computerFresh, computerOffline, onReplaced, orderedRows, setDraftKeys } from "./catalog";
import { composer } from "./composer";
import { sessionModels, bindModelNotice } from "./sessionModels";
import { bindForkSession } from "./sessionForks";
import { tasks } from "./tasks";
import { terminal } from "./terminal";
import { flag, num } from "./settings";
import { bindActivity, bindSelection, followedFor, notify, updating } from "./actions";
import { observeMarks } from "./marks";
import { createActivity } from "./activity";
import { createSubagentActivity } from "./subagentActivity";
import { archive } from "./archive";
import { applyGrouping } from "./organization";
import { activityReadOnly } from "./activitySource";
import { compactRows, detailTab, page, selected, selectedKey, setCompactRows, setDetailTab, setReadSelectionKey, setSelectedKey, subagentOpen } from "./ui";
import { startScreens } from "./screens";
import { documentVisibility } from "./visibility";
import { processes, trackFollow, trackShown } from "./processes";
import type { Batch, ComposerEvent } from "../types";

// Wires the stores together: batches go to the catalog, the grouping, the
// followed Activity and the open composer, the selected session is the
// followed one and has the open composer, and a
// `replaced` run hands its selection and row layout to the new run of the
// same name. An archived row is never followed (Qt streams only rows
// without an archive id); store/archiveActivity.ts reads its recorded
// Activity.

export const activity = createActivity(activityPort);
bindActivity(activity);
bindModelNotice((message) => notify("error", message, true));
bindSelection(() => ({ key: selectedKey(), subagent: subagentOpen() }));
bindForkSession({
  busy: (row) => sessionModels.changing() || (composer.view()?.key === `${row.computer}\n${row.name}` && (!!composer.view()?.sending || !!composer.view()?.waiting)),
  terminal: () => { terminal.opened(); setDetailTab("terminal"); },
});

/** The selected child's own bounded, read-only inspection page. */
export const subagentActivity = createRoot(() => {
  const store = createSubagentActivity({
    read: (target) => inspectSubagentActivity(
      { computer: target.computer, name: target.name, run_id: target.runId, conversation_id: target.conversationId },
      target.archiveId,
      target.agentId,
    ),
    showing: () => page() === "sessions" && !documentVisibility.hidden(),
    online: (computer) => !computerOffline(computer),
  });
  createEffect(() => {
    const row = selected(), agentId = subagentOpen();
    const target = row && agentId && row.runId && row.conversationId ? {
      id: row.key, computer: row.computer, name: row.name, runId: row.runId, conversationId: row.conversationId,
      archiveId: row.archiveId ?? null, agentId,
    } : undefined;
    untrack(() => store.follow(target));
  });
  return store;
});

/** Applies one batch from the bridge: its rows and their grouping are one reactive update. */
export function receive(b: Batch) {
  batch(() => {
    if (b.desktop_service) desktopService.apply(b.desktop_service);
    applyBatch(b);
    applyGrouping(b);
    archive.applyGroups(b.groups ?? []);
  });
  observeMarks(b);
  if (b.activity.length) activity.applyActivity(b.activity);
  if (b.tasks?.length) tasks.apply(b.tasks);
  if (b.processes_view) processes.apply(b.processes_view);
  if (b.composer) composer.receive(b.composer);
  if (b.draft_keys) setDraftKeys(b.draft_keys);
  for (const e of b.composer_events ?? []) composerEvent(e);
}

/** What Rust says after a call ended: Qt's showNotice, or the Terminal tab its receipt asks for. */
function composerEvent(e: ComposerEvent) {
  // An error stays until dismissed unless it is transient (showNotice, SessionsWindow.cpp:5210).
  if (e.kind === "notice") notify(e.error ? "error" : "info", e.text, e.error && !e.transient);
  // Codex's hooks review and Update now continue in Terminal (1157-1178); showing the tab connects it.
  else if (composer.view()?.key === e.key) { terminal.opened(); setDetailTab("terminal"); }
}

createRoot(() => {
  // Another selection lets the row whose reply was just read leave the
  // filtered list (Qt's selectSession and selectCreation clear m_readSelectionKey).
  createEffect(on(selectedKey, () => { setReadSelectionKey(undefined); }, { defer: true }));
  // The selected session's composer. A rename keeps the catalog key, so Rust
  // moves its draft to the new name. An archived row opens its own editable
  // draft too; only its recorded Activity stays read-only (Qt selectSession).
  createEffect(on(() => {
    const s = selected();
    return s ? JSON.stringify([s.computer, s.catalogKey, s.name, s.state === "archived" ? s.archiveId ?? "" : "", subagentOpen(), s.conversationId ?? ""]) : undefined;
  }, (identity) => {
    const s = selected(), child = subagentOpen();
    const opened = composer.open(identity && s ? {
      computer: s.computer, key: s.catalogKey, name: s.name, archive_id: s.state === "archived" ? s.archiveId ?? null : null,
      subagent: child ? { agent_id: child, parent_conversation_id: s.conversationId ?? "" } : undefined,
    } : undefined);
    // The first new-run frame may be its only state. Rust must bind the
    // composer's followed key before that frame can reach its observer.
    // Display/routing moves immediately; only the original subscribe waits.
    activity.follow(s && !activityReadOnly(s.key) ? { id: s.key, computer: s.computer, key: s.catalogKey, name: s.name, runId: s.runId, conversationId: s.conversationId } : undefined, opened);
  }));
  // The detail tab that shows: behind Terminal a failed read waits Qt's
  // background interval (Settings > Processes), which Rust reads when told.
  createEffect(on(() => [detailTab(), num("processes/backgroundSeconds"), num("processes/activeSeconds")] as const, ([tab]) => {
    void composerTab(tab === "taskview" ? "native" : tab).catch(() => {});
  }));
});

// Model settings reuse the selected session's existing Activity inspection and
// bounded pending keys. There is no model-settings poller or catalog-wide scan.
createRoot(() => {
  createEffect(() => {
    const row = selected(), v = composer.view();
    const followed = row ? followedFor(row.key) : undefined;
    const context = row && !subagentOpen() ? { row, details: followed?.value, online: !computerOffline(row.computer),
      busy: updating(row.computer, row.name) || !!v?.sending || !!v?.waiting,
      answering: !!v?.question?.answering, inspectError: !!activity.error() || !!v?.inspect_error } : undefined;
    untrack(() => sessionModels.setContext(context));
  });
  createEffect(() => {
    sessionModels.view();
    const rows = sessionModels.queuedKeys().map((key) => {
      const row = catalog.rows[key];
      return { key, row, online: !!row && !computerOffline(row.computer), busy: !!row && updating(row.computer, row.name) };
    });
    untrack(() => sessionModels.observe(rows));
  });
});

// The Processes tab follows the selected session while Settings > Processes
// is on (Qt polls nothing while it is off); output reads and the busy
// indicator run only while the tab shows in a shown window.
const [windowHidden, setWindowHidden] = createSignal(windowVisibility.hidden());
windowVisibility.onChange(() => setWindowHidden(windowVisibility.hidden()));
createRoot(() => {
  const enabled = () => flag("processes/enabled");
  trackFollow({
    enabled, selected,
    facts: (s) => {
      const host = followedFor(s.key).value?.host_generation;
      return {
        online: computerFresh(s.computer),
        generation: typeof host === "string" && host ? host : null,
        inspectFailed: activity.target()?.id === s.key && !!activity.error(),
      };
    },
    follow: processesFollow,
  });
  trackShown({ enabled, selected: () => !!selected(), page, tab: detailTab, hidden: windowHidden, shown: processesShown });
});

// `?select=<session name>` opens that session once it is listed (screenshots, scripted measurements).
const wanted = new URLSearchParams(location.search).get("select");
if (wanted) {
  createRoot((dispose) => {
    createEffect(() => {
      const row = orderedRows().find((s) => s.name === wanted);
      if (!row) return;
      if (!selectedKey()) setSelectedKey(row.key);
      dispose();
    });
  });
}

onReplaced((from, to) => {
  if (compactRows[from]) setCompactRows(to, true);
  if (selectedKey() === from) setSelectedKey(to);
});

// The Overview, Projects, Machines and Accounts reads run while their page shows.
startScreens(page, documentVisibility);
