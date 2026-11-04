import { describe, expect, it } from "vitest";
import catalog from "./i18n.ru.json";
import english from "./i18n.english.json";
import supplement from "./i18n.next-ru.json";

// Every static tr()/trNoop() source is a reviewed Qt catalog entry, a reviewed
// Next-only entry (i18n.next-ru.json, merged by the generator) or an explicit
// English-only string (i18n.english.json, read by this test only).
// tr() of a value stays in the files below: their values come from their own
// trNoop() tables or from literal component props, never from session text,
// agent output, identities or file names.
const DYNAMIC_FILES: Record<string, string> = {
  "./App.tsx": "connection state names from its STATE_TEXT table",
  "./ui/components/Rail.tsx": "its own screen table",
  "./ui/screens/Machines.tsx": "field labels passed as literal props",
  "./ui/screens/MachineConnectionDialog.tsx": "its own option tables",
  "./ui/screens/Settings.tsx": "the section table",
  "./ui/screens/settings/ui.tsx": "titles, labels and hints passed as literal props",
  "./ui/sessions/SessionList.tsx": "its filter and scope tables",
  "./ui/sessions/SessionCard.tsx": "the core's closed caption vocabulary and child states",
  "./ui/sessions/Inspector.tsx": "its own tab table and actions-ui identity titles",
  "./ui/sessions/ActionDialogs.tsx": "actions-ui menu labels, reasons and notices; detail reasons from the core",
  "./ui/sessions/mark-entries.ts": "the actions store notice constants",
  "./store/actions.ts": "its own notice constants",
  "./store/restores.ts": "the actions notice constants",
  "./store/taskBoard.ts": "its own task action table",
  "./ui/screens/tasks-ui.ts": "its own task stage table",
  "./ui/screens/Tasks.tsx": "the stage tile table and the task pause reason",
  "./ui/screens/TaskInspector.tsx": "the task pause reason",
  "./ui/screens/TaskWorkflow.tsx": "the task pause reason",
  "./ui/sessions/SavedDrafts.tsx": "the Saved drafts store's TEXTS, REASONS and SECTIONS tables and its own Show more table",
  "./ui/sessions/ArchiveBar.tsx": "the archive period table",
  "./ui/sessions/archive-dates.ts": "the archive period table",
  "./ui/sessions/opencode-models.ts": "its credential kind table",
  "./ui/sessions/session-usage-ui.ts": "its own token and time field tables",
  "./ui/sessions/transcript.ts": "its own event title, voice state and local stamp tables",
  "./ui/sessions/Activity.tsx": "the transcript's voice state table",
  "./ui/sessions/shortcutHelp.ts": "the shortcut registry's command titles and its own group table",
  "./ui/screens/settings/Keyboard.tsx": "the shortcut registry's command titles and groups",
  "./store/goTo.ts": "its own section table and the shortcut registry's command titles and groups",
  "./store/archive.ts": "its own notice constant",
  "./store/composer.ts": "its own error constants; core status text keeps its source form",
  "./store/contentSearch.ts": "its own notice constant",
  "./store/creations.ts": "the creations-ui notice constant",
  "./store/drops.ts": "its own drop hint constants",
  "./store/endActions.ts": "its own notice constant and its title and cancel tables",
  "./store/forks.ts": "the fork-ui notice constant",
  "./store/native-ui-model.ts": "its own status constants",
  "./store/organization.ts": "its own notice and reason constants",
  "./store/taskCancel.ts": "its own notice constant",
  "./ui/components/folder-browser.ts": "its own status and button constants",
  "./ui/components/MachineTile.tsx": "its own state table",
  "./ui/screens/project-folders.ts": "its own notice constant",
  "./ui/screens/settings/analytics-text.ts": "its own computer name constant",
  "./ui/screens/settings/Appearance.tsx": "its own variant table",
  "./ui/screens/settings/ApprovalsConfirm.tsx": "the approval opt-in constants",
  "./ui/screens/settings/approvals-journal.ts": "its own placeholder constant",
  "./ui/screens/settings/Approvals.tsx": "the approval opt-in and journal constants",
  "./ui/screens/settings/Sessions.tsx": "the native notification controller's closed unavailable, unconfirmed and not-applied messages",
  "./ui/screens/settings/computer-invitations.ts": "its own notice constant",
  "./ui/screens/settings/connector-actions.ts": "the workspace enrollment notice constants",
  "./ui/screens/settings/legacy-relay.ts": "its own notice constant",
  "./ui/screens/settings/Recovery.tsx": "its own help constant and recovery settings status/error prose",
  "./ui/screens/settings/Voice.tsx": "its own provider help table",
  "./ui/screens/settings/workspace-enrollment.ts": "its own notice constants",
  "./ui/screens/settings/WorkspacePanel.tsx": "the workspace enrollment notice constant",
  "./ui/screens/WorktreeOverviewDialog.tsx": "the overview state's category table",
  "./ui/sessions/CreationCard.tsx": "the creations-ui menu constant",
  "./ui/sessions/CreationPanel.tsx": "the creations-ui button table",
  "./ui/sessions/creations-ui.ts": "its own status and menu constants",
  "./ui/sessions/LaunchWarning.tsx": "the launch warning title and button table",
  "./ui/sessions/local-actions.ts": "its own reason constants",
  "./ui/sessions/machine-batch.ts": "its own menu constant",
  "./ui/sessions/newsession-ui.ts": "its own New project row constant",
  "./ui/sessions/selection-live.ts": "the machine-batch menu constant",
  "./ui/sessions/selection.ts": "its own action text table",
  "./ui/sessions/selection-ui.ts": "the selection hint constant",
};

const SOURCES = import.meta.glob(["./**/*.ts", "./**/*.tsx", "!./**/*.test.ts", "!./i18n.ts"], { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const STATIC = /\btr(?:Noop)?\(\s*("(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`[^`$]*`)/g;
const DYNAMIC = /\btr\(\s*(?!["'])(?!`[^`$]*`)/;
// Qt's tr(source, disambiguation): both literal, keyed "disambiguation\u0004source".
const DISAMBIGUATED = /\btrc\(\s*("(?:[^"\\\n]|\\.)*")\s*,\s*("(?:[^"\\\n]|\\.)*")/g;

function literal(token: string): string {
  if (token.startsWith("`")) return token.slice(1, -1);
  if (token.startsWith("'")) return JSON.parse(`"${token.slice(1, -1).replace(/\\'/g, "'").replace(/"/g, '\\"')}"`);
  return JSON.parse(token);
}

const sources = new Map<string, string[]>();
for (const [file, text] of Object.entries(SOURCES)) {
  for (const match of text.matchAll(STATIC)) {
    const source = literal(match[1]);
    sources.set(source, [...(sources.get(source) ?? []), file]);
  }
}
const disambiguated = new Map<string, string>();
for (const [file, text] of Object.entries(SOURCES)) {
  for (const match of text.matchAll(DISAMBIGUATED)) disambiguated.set(`${literal(match[1])}\u0004${literal(match[2])}`, file);
}
// Prose the Rust core and app mark with text!/source! (their own tests check
// the catalog has them); a supplement entry may serve only those.
const RUST = import.meta.glob(["../core/src/**/*.rs", "../src-tauri/src/**/*.rs"], { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const MARKED = /\b(?:text|source)!\(\s*("(?:[^"\\\n]|\\.)*")/g;
const marked = new Set(Object.values(RUST).flatMap((text) => [...text.matchAll(MARKED)].map((match) => JSON.parse(match[1]) as string)));
const translated = catalog as Record<string, unknown>;
const englishOnly = new Set(english as string[]);

describe("interface string coverage", () => {
  it("translates every wrapped literal or lists it as English-only", () => {
    const missing = [...sources].filter(([source]) => !(source in translated) && !englishOnly.has(source)).map(([source, files]) => `${JSON.stringify(source)} in ${files[0]}`);
    expect(missing).toEqual([]);
  });

  it("translates every disambiguated literal", () => {
    expect([...disambiguated].filter(([key]) => !(key in translated)).map(([key, file]) => `${JSON.stringify(key)} in ${file}`)).toEqual([]);
  });

  it("keeps the English-only list current", () => {
    expect([...englishOnly].filter((source) => source in translated)).toEqual([]);
    expect([...englishOnly].filter((source) => !sources.has(source))).toEqual([]);
    expect(english as string[]).toEqual([...english as string[]].sort());
  });

  it("keeps only used sources in the Next-only supplement", () => {
    expect(Object.keys(supplement).filter((source) => !sources.has(source) && !marked.has(source))).toEqual([]);
  });

  it("translates computed values only in reviewed files", () => {
    const unreviewed = Object.entries(SOURCES).filter(([file, text]) => DYNAMIC.test(text) && !(file in DYNAMIC_FILES)).map(([file]) => file);
    expect(unreviewed).toEqual([]);
  });
});
