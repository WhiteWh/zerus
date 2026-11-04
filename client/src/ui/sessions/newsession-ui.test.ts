import { describe, expect, it } from "vitest";
import { machineMeta, NOW, usageOf } from "../../store/testing";
import type { Account, FolderOut, Profile, ProjectedWindow, Usage } from "../../types";
import {
  accountRows, computerChoice, computerRows, folderRows, launchSummary, launchWindows, projectChoice, projectRows, type AccountChoice, type Fleet,
  type ListedProject, type ProjectFolders,
} from "./newsession-ui";
import { folderLaunchProblem } from "./folder-launch";

// The New session dialog's view rules (tray/src/NewSessionDialog.cpp:
// loadMachines 487-510, setGroups 512-521, loadFolders 536-580,
// updateAccounts 876-952, updateUsage 1002-1023; AccountUsage.h 130-142;
// ProjectsDialog.cpp 523-532). Synthetic computers and paths only.

const fleet = (extra: Partial<Fleet> = {}): Fleet => ({
  local: machineMeta("pc"), peers: ["mac"], online: () => true, ...extra,
});

/** A folder as the organization holds it (`FolderOut`): its key is its id, else its computer and path. */
const folder = (id: string, machine: string, path: string, extra: Partial<FolderOut> = {}): FolderOut =>
  ({ id, key: id || `${machine}\n${path}`, machine, path, name: "", machine_id: "", machine_name: "", ...extra });

const project = (id: string, folders: FolderOut[] = [], extra: Partial<ListedProject> = {}): ListedProject & ProjectFolders =>
  ({ id, name: id, color: null, accessible: true, palette: 0, folders, ...extra });

const window = (used: number, minutes: number | null, extra: Partial<ProjectedWindow> = {}): ProjectedWindow =>
  ({ id: `w${minutes}`, label: "", used, minutes, resets_at: NOW + 3600, ...extra });

function member(id: string, provider: string, extra: Partial<Profile> = {}): Profile {
  return {
    key: `${provider}:${id}`, host: "", machine: "pc", id, label: "Work", provider, native: id.startsWith("native-"), installed: true,
    is_default: false, home: `/profiles/${id}`, auth_revision: "", online: true, state: "ready", detail: { source: "" }, signed_in: true,
    permission: "", permission_label: { source: "" }, permission_hint: { source: "" }, model_supported: false, model_text: { source: "" },
    usage: usageOf({ provider }), opencode: null, ...extra,
  };
}

function account(members: Profile[], usage: Partial<Usage> = {}): Account {
  const first = members[0];
  return {
    key: `${first.provider}:${first.id}`, label: first.label, provider: first.provider, installed: true, machines: ["pc"], members,
    usage: usageOf({ provider: first.provider, ...usage }), permission: { source: "" }, identity_lines: [], identity_note: null,
  };
}

const choice = (extra: Partial<AccountChoice> = {}): AccountChoice => ({
  host: "", agent: "codex", account: "", preset: false, current: undefined, remembered: {}, loading: false, style: "posix", now: NOW, ...extra,
});

// The fake hgs catalog of test_newsessiondialog.cpp: work and other (the default) are Codex, claude is Claude, native-dsh DeepSeek.
const catalog = () => [
  account([member("work", "codex")]),
  account([member("other", "codex", { is_default: true })]),
  account([member("claude", "claude", { label: "Review" })]),
  account([member("native-dsh", "dsh", { label: "DeepSeek", native: true })]),
];

const ids = (rows: { id: string; provider: string }[]) => rows.map((r) => `${r.provider}:${r.id}`);

describe("computers (loadMachines 487-510)", () => {
  it("lists this computer, the peers, the previous choice and the project's computers", () => {
    const rows = computerRows(fleet({ peers: ["mac", "studio"] }), "build-01", project("p", [folder("f1", "lab", "/srv/a"), folder("f2", "pc", "/srv/b")]));
    expect(rows.map((r) => r.target)).toEqual(["", "mac", "studio", "build-01", "lab"]);
    expect(rows[0].label).toBe("pc");
  });

  it("says (offline) after a computer whose card is not ok (ruling D5)", () => {
    const rows = computerRows(fleet({ online: (computer) => computer !== "mac" }), "", undefined);
    expect(rows.map((r) => r.label)).toEqual(["pc", "mac (offline)"]);
  });

  it("leaves this computer out on a control console, and unconfigured identified folders", () => {
    const console = fleet({ local: machineMeta("pc", {}, { local_sessions: false }) });
    expect(computerRows(console, "", project("p", [folder("f1", "pc", "/srv/a")])).map((r) => r.target)).toEqual(["mac"]);
    const identified = project("p", [folder("f1", "lab", "/srv/a", { machine_id: "m-9" }), folder("f2", "mac", "/srv/b", { machine_id: "m-2" })]);
    expect(computerRows(fleet(), "", identified).map((r) => r.target)).toEqual(["", "mac"]);
    // Before this computer's identity arrives there is no row for it.
    expect(computerRows(fleet({ local: machineMeta("") }), "", undefined).map((r) => r.target)).toEqual(["mac"]);
  });

  it("keeps the previous choice, else the first row", () => {
    const rows = computerRows(fleet(), "", undefined);
    expect(computerChoice(rows, "mac")).toBe(1);
    expect(computerChoice(rows, "gone")).toBe(0);
    expect(computerChoice([], "")).toBe(-1);
  });
});

describe("projects (setGroups 512-521)", () => {
  const projects = [project("inbox", [], { name: "Inbox" }), project("work", [], { name: "Work", palette: 3 }), project("hidden", [], { accessible: false })];

  it("lists accessible projects and New project… for a repository", () => {
    expect(projectRows(projects, false).map((r) => r.name)).toEqual(["Inbox", "Work"]);
    const rows = projectRows(projects, true);
    expect(rows.map((r) => r.name)).toEqual(["Inbox", "Work", "New project…"]);
    expect(rows[2]).toMatchObject({ id: "", newProject: true });
    expect(rows[1]).toEqual({ id: "work", name: "Work", color: null, palette: 3, newProject: false });
  });

  it("keeps New project…, then the selected one, the initial name and the default", () => {
    const rows = projectRows(projects, true);
    expect(projectChoice(rows, { keepNew: true, selected: "", initial: "", fallback: "inbox" })).toBe(2);
    expect(projectChoice(rows, { keepNew: false, selected: "work", initial: "", fallback: "inbox" })).toBe(1);
    expect(projectChoice(rows, { keepNew: false, selected: "gone", initial: "Work", fallback: "inbox" })).toBe(1);
    expect(projectChoice(rows, { keepNew: false, selected: "", initial: "", fallback: "inbox" })).toBe(0);
  });
});

describe("folders (loadFolders 536-580)", () => {
  const example = project("p", [folder("f1", "pc", "/workspace/example"), folder("f2", "mac", "/workspace/remote"), folder("f3", "pc", "/workspace/docs/", { name: "Docs" })]);

  it("lists the project's folders on the computer, then a custom path", () => {
    const plain = folderRows(example, "", "pc", "posix", "");
    expect(plain.rows.map((r) => [r.key, r.path, r.name])).toEqual([["f1", "/workspace/example", "example"], ["f3", "/workspace/docs", "Docs"]]);
    expect(plain.index).toBe(0);
    const listed = folderRows(example, "", "pc", "posix", "/workspace/docs");
    expect(listed.index).toBe(1);
    expect(listed.rows.length).toBe(2);
    const custom = folderRows(example, "", "pc", "posix", "/workspace/other/");
    expect(custom.rows[2]).toEqual({ key: "", path: "/workspace/other", name: "other", tooltip: "/workspace/other" });
    expect(custom.index).toBe(2);
    expect(folderRows(example, "mac", "pc", "posix", "").rows.map((r) => r.key)).toEqual(["f2"]);
    // A folder without an id is listed by its key: its computer and path.
    const unnamed = project("p", [folder("", "pc", "/workspace/plain")]);
    expect(folderRows(unnamed, "", "pc", "posix", "").rows[0]).toMatchObject({ key: "pc\n/workspace/plain", name: "plain" });
    expect(folderRows(undefined, "", "pc", "posix", "").index).toBe(-1);
  });

  it("names Windows folders natively and compares them ignoring case", () => {
    const windows = project("p", [folder("f1", "pc", "C:/Work/Example")]);
    expect(folderRows(windows, "", "pc", "windows", "c:\\work\\example").index).toBe(0);
    const root = folderRows(windows, "", "pc", "windows", "D:\\");
    expect(root.rows[1]).toEqual({ key: "", path: "D:/", name: "D:\\", tooltip: "D:\\" });
  });
});

describe("account rows (updateAccounts 876-952)", () => {
  it("selects the agent's default, and the shell row comes last", () => {
    const { rows, selected } = accountRows(catalog(), choice());
    expect(ids(rows)).toEqual(["codex:work", "codex:other", "claude:claude", "dsh:native-dsh", "sh:"]);
    expect(rows[selected].id).toBe("other");
    expect(rows[4]).toMatchObject({ shell: true, name: "Run shell", summary: "Interactive terminal session", accessible: "Run shell" });
    expect(accountRows(catalog(), choice({ style: "windows" })).rows[4].name).toBe("Run PowerShell");
  });

  // unavailableRememberedAgentSelectsFirstAgent: a remembered agent this computer lacks falls back to the first row.
  it("falls back to the first row for an agent the computer lacks", () => {
    const { rows, selected } = accountRows(catalog(), choice({ agent: "kimi" }));
    expect(selected).toBe(0);
    expect(rows[0].provider).toBe("codex");
  });

  // rememberedAndExplicitMissingAccounts.
  it("never replaces an explicit account, but a remembered one falls back", () => {
    expect(accountRows(catalog(), choice({ remembered: { codex: "gone" } })).selected).toBe(1);
    expect(accountRows(catalog(), choice({ remembered: { codex: "work" } })).selected).toBe(0);
    expect(accountRows(catalog(), choice({ preset: true, account: "gone" })).selected).toBe(-1);
    expect(accountRows(catalog(), choice({ preset: true, account: "work" })).selected).toBe(0);
    expect(accountRows(catalog(), choice({ agent: "sh", preset: true, account: "" })).selected).toBe(4);
  });

  it("keeps the current row of this computer and agent", () => {
    const rows = accountRows(catalog(), choice({ current: { host: "", provider: "codex", id: "work" } }));
    expect(rows.selected).toBe(0);
    // Another computer's current row is no choice here.
    expect(accountRows(catalog(), choice({ current: { host: "mac", provider: "codex", id: "work" } })).selected).toBe(1);
  });

  // focusWhileCatalogLoadsKeepsRememberedAgent and pendingAccountsAllowShellOnlyWhenChosen.
  it("chooses nothing while the computer's catalog loads with no profiles", () => {
    expect(accountRows([], choice({ agent: "claude", loading: true }))).toMatchObject({ selected: -1 });
    expect(accountRows([], choice({ agent: "sh", loading: true })).selected).toBe(0);
    expect(accountRows([], choice({ agent: "claude" })).selected).toBe(0);
  });

  // accountAliasesMatchAccountsGroupingAndKeepExactBinding: one row per account, bound to the ranked member.
  it("ranks a group's members: preset, current, remembered, default, native", () => {
    const shared = [account([member("claude", "claude", { label: "Review" }), member("claude-copy", "claude", { label: "Review", is_default: true })])];
    expect(accountRows(shared, choice({ agent: "claude" })).rows[0].id).toBe("claude-copy");
    expect(accountRows(shared, choice({ agent: "claude", remembered: { claude: "claude" } })).rows[0].id).toBe("claude");
    expect(accountRows(shared, choice({ agent: "claude", preset: true, account: "claude" })).rows[0].id).toBe("claude");
    expect(accountRows(shared, choice({ agent: "claude", current: { host: "", provider: "claude", id: "claude" } })).rows[0].id).toBe("claude");
    const native = [account([member("a", "codex"), member("native-codex", "codex", { native: true })])];
    expect(accountRows(native, choice()).rows[0].id).toBe("native-codex");
  });

  // pairsAndLimitsLaunchExactProfile and unknownAndEndedWindowsStayUnavailable.
  it("names each row by its identity and sums up its limits", () => {
    const limited = [account([member("work", "codex")], { identity: { ...usageOf().identity, email: "shared@example.invalid" }, windows: [window(17, 300), window(78, 10080)] })];
    const row = accountRows(limited, choice()).rows[0];
    expect(row.name).toBe("shared@example.invalid");
    expect(row.summary).toBe("5h 17% used  7d 78% used");
    expect(row.tooltip).toBe("shared@example.invalid\n5h 17% used  7d 78% used");
    expect(row.accessible).toBe("Codex shared@example.invalid\n5h 17% used  7d 78% used");
    expect(row.tooltip).not.toContain("Reset in");
    const unknown = accountRows([account([member("work", "codex", { label: "" })])], choice()).rows[0];
    expect(unknown.tooltip).toBe("Account n/a\nlimits n/a");
    expect(accountRows([account([member("d", "codex", { label: "Default account" })])], choice()).rows[0].name).toBe("Account n/a");
    // The cached account status names it while the inspect has no identity.
    expect(accountRows(catalog(), choice({ cachedEmail: (provider, id) => (id === "work" ? "cached@example.invalid" : "") })).rows[0].name).toBe("cached@example.invalid");
  });

  it("tones a row by its severest current window, except offline or after a failed refresh", () => {
    const usage = { windows: [window(95, 300)] };
    expect(accountRows([account([member("w", "codex")], usage)], choice()).rows[0].tone).toBe("critical");
    expect(accountRows([account([member("w", "codex")], { ...usage, offline: true })], choice()).rows[0].tone).toBeUndefined();
    expect(accountRows([account([member("w", "codex")], { ...usage, refresh_error: true })], choice()).rows[0].tone).toBeUndefined();
    expect(accountRows([account([member("w", "codex")], { windows: [window(10, 300)] })], choice()).rows[0].tone).toBeUndefined();
  });
});

describe("launchSummary (AccountUsage.h 130-142)", () => {
  it("lists current windows, then balances, else limits n/a", () => {
    expect(launchSummary(usageOf({ windows: [window(17.4, 300), window(100.6, 10080)] }), NOW)).toBe("5h 17% used  7d 100% used");
    expect(launchSummary(usageOf({ windows: [window(50, 300, { resets_at: NOW - 60 })] }), NOW)).toBe("limits n/a");
    expect(launchSummary(usageOf({ windows: [window(40, null, { label: "Monthly", resets_at: null })] }), NOW)).toBe("Monthly 40% used");
    expect(launchSummary(usageOf({ windows: [window(40, 300, { label: "Sonnet" })] }), NOW)).toBe("5h 40% used");
    expect(launchSummary(usageOf({ balances: [{ kind: "", balance: "12.50", currency: "USD" }, { kind: "", balance: "n/a", currency: "USD" }] }), NOW)).toBe("Balance: 12.50 USD");
    expect(launchSummary(usageOf({ balances: [{ kind: "", balance: "3", currency: "" }] }), NOW)).toBe("Balance: 3");
    expect(launchSummary(usageOf(), NOW)).toBe("limits n/a");
  });
});

describe("launchWindows (AccountUsage.h 117-129)", () => {
  // The account row's compact meters: current windows with a value, clamped, a sized window without its name.
  it("keeps the current windows the row draws as meters", () => {
    const usage = usageOf({ windows: [window(17.4, 300, { label: "Sonnet" }), window(120, 10080), window(50, 300, { resets_at: NOW - 60 }), window(Number.NaN, 300), window(40, null, { label: "Monthly", resets_at: null })] });
    expect(launchWindows(usage, NOW)).toEqual([
      { minutes: 300, used: 17.4, label: undefined, resetsIn: 60 },
      { minutes: 10080, used: 100, label: undefined, resetsIn: 60 },
      { minutes: null, used: 40, label: "Monthly", resetsIn: null },
    ]);
    // Without one the row shows its summary text instead.
    expect(launchWindows(usageOf(), NOW)).toEqual([]);
    expect(launchWindows(undefined, NOW)).toEqual([]);
  });
});

describe("folderLaunchProblem (ProjectsDialog.cpp 523-532)", () => {
  const env = { localHost: "pc", online: (computer: string) => computer !== "mac", style: () => "posix" as const };

  it("names the four problems", () => {
    expect(folderLaunchProblem({ ...folder("", "pc", "/srv/a"), key: "" }, env, true)).toBe("Select a folder for the new session.");
    // Qt makes up an id for a folder without one (SessionOrganization.cpp:42); here its key stands for it.
    expect(folderLaunchProblem(folder("", "pc", "/srv/a"), env, true)).toBe("");
    expect(folderLaunchProblem(folder("f", "", "/srv/a"), env, true)).toBe("Waiting for this computer's identity.");
    expect(folderLaunchProblem(folder("f", "pc", "/srv/a"), { ...env, localHost: "" }, true)).toBe("Waiting for this computer's identity.");
    expect(folderLaunchProblem(folder("f", "mac", "/srv/a"), env, true)).toBe("Reconnect mac to start a session in this folder.");
    expect(folderLaunchProblem(folder("f", "pc", "/srv/a"), env, false)).toBe("This folder is unavailable on this computer.");
    expect(folderLaunchProblem(folder("f", "studio", "relative"), env, undefined)).toBe("This folder is unavailable on this computer.");
    expect(folderLaunchProblem(folder("f", "pc", "/srv/a"), env, true)).toBe("");
    expect(folderLaunchProblem(folder("f", "studio", "/srv/a"), env, undefined)).toBe("");
  });
});
