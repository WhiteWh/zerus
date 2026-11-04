import { createRoot } from "solid-js";
import * as bridge from "../bridge";
import { catalog, computerFresh, orderedRows } from "./catalog";
import { archive } from "./archive";
import { notify, perform, updating } from "./actions";
import { running } from "./batch";
import { machineCards } from "./machines";
import { defaultProject, groupOf, grouping, groups, memberOf, moveToProject, setProjectCollapsed } from "./organization";
import { adoptSession } from "./creations";
import { setPage, setSubagentOpen } from "./ui";
import { createForks } from "./forks";
import type { ActionRow } from "../ui/sessions/actions-ui";
import { tr } from "../i18n";

let context = { busy: (_row: ActionRow) => false, terminal: () => {} };
/** Session wiring supplies composer state and Terminal without importing them into menus. */
export function bindForkSession(value: typeof context) { context = value; }

const rows = () => [...orderedRows(), ...archive.rows()];
const current = (key: string) => catalog.rows[key] ?? archive.row(key);
export const sessionForks = createRoot(() => createForks({
  rows, current,
  online: computerFresh,
  busy: (row) => running() || updating(row.computer, row.name) || context.busy(row),
  inspect: (row) => bridge.inspectSession({ computer: row.computer, name: row.name, run_id: row.runId ?? "", conversation_id: row.conversationId ?? "" }, row.archiveId ?? null).then((details) => details ?? undefined),
  warning: (computer) => {
    const local = machineCards[""];
    // A known local Windows host alone can require Qt's launch warning.
    return local ? bridge.launchWarning(local, computer) : Promise.resolve(null);
  },
  perform: (row, action) => perform(row, action, { quiet: true }),
  groupOf, defaultGroup: defaultProject,
  hasGroup: (group) => grouping().headers.some((h) => h.id === group),
  next: (source, group) => {
    const rank = (key: string) => groups[key]?.rank ?? archive.row(key)?.rank ?? Number.MAX_SAFE_INTEGER;
    const members = rows().filter((r) => groupOf(r.key) === group).sort((a, b) => rank(a.key) - rank(b.key));
    const index = members.findIndex((r) => r.key === source.key);
    return index >= 0 ? members[index + 1] : undefined;
  },
  place: (row, group, before) => {
    if (before && memberOf(before) === memberOf(row)) return;
    void moveToProject([row], group, before).then((out) => {
      if (out.outcome !== "refused") notify("info", tr("The fork is placed after its original and opens Terminal"), false, "session-fork", "Forked");
    });
    setProjectCollapsed(group, false);
  },
  select: (row) => { setSubagentOpen(undefined); adoptSession(row.key); setPage("sessions"); context.terminal(); },
  notify: (tone, text, sticky) => { notify(tone, text, sticky, "session-fork"); },
}));
