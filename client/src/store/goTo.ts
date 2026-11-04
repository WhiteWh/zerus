import { createSignal } from "solid-js";
import { tr, trNoop } from "../i18n";
import { nativeOverlayBarrier } from "./ui";
import { COMMANDS, chord, handlerFor, popupOpen } from "./shortcuts";
import { openSession } from "./desk";
import { accountsPage, setAccountMachine, setRequestedProfile } from "./accounts";
import { requestMachine } from "./machines";
import { groupOf, grouping, headerOf, setProjectToShow } from "./organization";
import { orderedRows } from "./catalog";
import { listRows, listedSessionIgnoringCollapse, machineChoices } from "../ui/sessions/model";
import { statusCaption } from "../ui/sessions/SessionCard";
import { GoToKind, recentSessionIds, type GoToItem } from "../ui/sessions/goTo";

const SECTIONS: readonly [string, string][] = [
  ["go.overview", trNoop("Overview")], ["go.sessions", trNoop("Sessions")], ["go.projects", trNoop("Projects")],
  ["go.machines", trNoop("Machines")], ["go.accounts", trNoop("Accounts")], ["go.settings", trNoop("Settings")],
];

export function goToItems(): GoToItem[] {
  const recent = recentSessionIds();
  const listed = new Map(listRows().flatMap((row, index) => row.type === "card" ? [[row.id, index] as const] : []));
  const sessions = orderedRows().filter((session) => session.state !== "archived");
  sessions.sort((a, b) => (listed.get(a.key) ?? Number.MAX_SAFE_INTEGER) - (listed.get(b.key) ?? Number.MAX_SAFE_INTEGER));
  const counts = new Map<string, number>();
  const items: GoToItem[] = sessions.map((session) => {
    const project = groupOf(session.key);
    counts.set(project, (counts.get(project) ?? 0) + 1);
    const group = headerOf(project)?.name;
    return {
      kind: GoToKind.Session, id: session.catalogKey, host: session.computer,
      title: session.title,
      detail: [group, session.machine, statusCaption(session)].filter((part): part is string => !!part),
      recency: recent.indexOf(session.key),
    };
  });
  for (const header of groupHeaders()) {
    if (!header.name) continue;
    items.push({ kind: GoToKind.Project, id: header.id, host: "", title: header.name, detail: [tr("Project"), tr("%n session(s)", counts.get(header.id) ?? 0)], recency: -1 });
  }
  for (const account of accountsPage().accounts) {
    for (const profile of account.members) {
      items.push({ kind: GoToKind.Account, id: profile.id, host: profile.host, title: profile.label || profile.id,
        detail: [tr("Account"), profile.provider, profile.machine].filter((part): part is string => !!part), recency: -1 });
    }
  }
  for (const machine of machineChoices()) {
    items.push({ kind: GoToKind.Machine, id: machine.computer, host: machine.computer, title: machine.label,
      detail: machine.computer ? tr("Machine") : [tr("Machine"), tr("this computer")], recency: -1 });
  }
  for (const [id, title] of SECTIONS) {
    items.push({ kind: GoToKind.Section, id, host: "", title: tr(title), detail: tr("Section"), chord: chord(id), recency: -1 });
  }
  for (const command of COMMANDS) {
    if (command.scope !== "window" || command.id.startsWith("go.")) continue;
    items.push({ kind: GoToKind.Command, id: command.id, host: "", title: tr(command.title), detail: tr(command.group), chord: chord(command.id), recency: -1 });
  }
  return items;
}

function groupHeaders() {
  return grouping().headers;
}

export const [goToOrigin, setGoToOrigin] = createSignal<HTMLElement | null>(null);
export const [shortcutHelpOrigin, setShortcutHelpOrigin] = createSignal<HTMLElement | null>(null);

export function openGoTo() {
  if (popupOpen()) return;
  const origin = document.activeElement as HTMLElement | null;
  const wait = nativeOverlayBarrier("goTo", true);
  if (wait) { void wait.then(() => setGoToOrigin(origin)).catch(() => {}); }
  else setGoToOrigin(origin);
}

export function closeGoTo(restore = true) {
  const origin = goToOrigin();
  setGoToOrigin(null);
  void nativeOverlayBarrier("goTo", false);
  if (restore && origin?.isConnected) origin.focus();
}

export function openShortcutHelp() {
  if (popupOpen()) return;
  const origin = document.activeElement as HTMLElement | null;
  const wait = nativeOverlayBarrier("shortcutHelp", true);
  if (wait) { void wait.then(() => setShortcutHelpOrigin(origin)).catch(() => {}); }
  else setShortcutHelpOrigin(origin);
}

export function closeShortcutHelp(restore = true) {
  const origin = shortcutHelpOrigin();
  setShortcutHelpOrigin(null);
  void nativeOverlayBarrier("shortcutHelp", false);
  if (restore && origin?.isConnected) origin.focus();
}

export function activateGoTo(item: GoToItem) {
  const origin = goToOrigin();
  closeGoTo(false);
  switch (item.kind) {
    case GoToKind.Session:
      openSession({ computer: item.host, key: item.id, preserveFilters: true }, { keepFiltersIf: listedSessionIgnoringCollapse });
      return;
    case GoToKind.Project: {
      const listed = listRows().find((row) => row.type === "card" && groupOf(row.id) === item.id);
      const first = listed?.type === "card" ? listed.session
        : orderedRows().find((session) => session.state !== "archived" && groupOf(session.key) === item.id);
      if (first) {
        activateGoTo({ ...item, kind: GoToKind.Session, id: first.catalogKey, host: first.computer });
      } else {
        handlerFor("go.projects", origin)?.();
        setProjectToShow(item.id);
      }
      return;
    }
    case GoToKind.Account:
      handlerFor("go.accounts", origin)?.();
      setRequestedProfile(item.id);
      setAccountMachine(item.host || "*");
      return;
    case GoToKind.Machine:
      handlerFor("go.machines", origin)?.();
      requestMachine(item.host);
      return;
    case GoToKind.Section:
    case GoToKind.Command:
      handlerFor(item.id, origin)?.();
  }
}

export function customizeShortcutHelp() {
  closeShortcutHelp(false);
  void import("../ui/screens/settings/sections").then(({ openSettings }) => {
    openSettings("keyboard");
    requestAnimationFrame(() => document.getElementById("set-nav-keyboard")?.focus());
  });
}
