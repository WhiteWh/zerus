import { describe, expect, it, vi } from "vitest";
import { machineMeta } from "./testing";
import { fleetKey } from "./newSession";
import { folder, NOT_LOADED, project, settle, setup, type Json, type World } from "./newSessionFake";
import type { Details, MachineMeta, NewSessionFormInput } from "../types";
import type { Session } from "../ui/sessions/types";

// tray/tests/test_newsessiondialog.cpp, ported over the recorded bridge of
// newSessionFake.ts; the OpenCode cases are in newSessionOpencode.test.ts.

vi.mock("./ui", () => ({ selected: () => undefined, machineFilter: () => [], setPage: () => {} }));
vi.mock("../bridge", () => ({}));
vi.mock("./creations", () => ({ creationAdmitted: () => {} }));

describe("accounts (NewSessionDialog.cpp 865-1023)", () => {
  // macOS gives the dialog's first focus to the list: while it loads, its only row (Run shell) is no choice.
  it("focusWhileCatalogLoadsKeepsRememberedAgent", async () => {
    const t = setup((w) => { w.context = { branch: "work-1a2b3c4d", own_windows_session: null, build_style: "posix", remembered: { agent: "claude", accounts: {}, open_terminal: false }, swarm: "" }; });
    t.world.hold("accounts:mac");
    await t.open({ computer: "mac" });
    expect(t.state().accounts.length).toBe(1);
    expect(t.state().account).toBe(-1);
    t.world.release("accounts:mac");
    await settle();
    expect(t.state().accounts[t.state().account].id).toBe("claude");
  });

  // A remembered agent this computer lacks falls back to the first agent, not the shell.
  it("unavailableRememberedAgentSelectsFirstAgent", async () => {
    const t = setup((w) => { w.flags.add("machine-accounts"); w.context = { branch: "work-1a2b3c4d", own_windows_session: null, build_style: "posix", remembered: { agent: "kimi", accounts: {}, open_terminal: false }, swarm: "" }; });
    await t.open();
    expect(t.row("pc-claude")).toBe(0);
    expect(t.state().account).toBe(0);
    expect(t.state().agent).toBe("claude");
  });

  it("rememberedAndExplicitMissingAccounts", async () => {
    const t = setup((w) => { w.context = { branch: "work-1a2b3c4d", own_windows_session: null, build_style: "posix", remembered: { agent: "", accounts: { "": { codex: "gone" } }, open_terminal: false }, swarm: "" }; });
    await t.open({ agent: "codex", path: "/workspace/example" });
    expect(t.state().accounts.length).toBe(5);
    expect(t.state().accounts[t.state().account].id).toBe("other");
    t.act.selectAccount("gone");
    await settle();
    expect(t.state().account).toBe(-1);
    expect(t.state().form?.start_enabled).toBe(false);
    expect(t.state().error).toContain("unavailable");
    await t.choose("work");
    t.world.flags.add("removed");
    t.act.refreshAccounts();
    await settle();
    expect(t.row("work")).toBe(-1);
    expect(t.state().account).toBe(-1);
    t.act.chooseAccount(t.state().accounts.length - 1);
    await settle();
    expect(t.state().form?.start_enabled).toBe(true);
    await t.start();
    expect(t.choices()).toMatchObject({ agent: "sh", account: "" });
  });

  it("refreshKeepsSelectionAndLastReportedLimits", async () => {
    const t = setup();
    await t.open({ agent: "codex" });
    expect(t.state().accounts.length).toBe(5);
    await t.choose("work");
    expect(t.state().accounts[t.row("work")].tooltip).toContain("17% used");
    t.world.flags.add("fail");
    t.act.refreshAccounts();
    await settle();
    expect(t.world.of("launchAccounts").at(-1)?.args).toEqual(["", true, "codex"]);
    expect(t.state().accountStatus).toContain("Could not load accounts");
    expect(t.state().accounts[t.state().account].id).toBe("work");
    expect(t.state().accounts[t.state().account].tooltip).toContain("17% used");
    expect(t.state().accounts.length).toBe(5);
    // The minute timer refreshes rows without reading the catalog again.
    const reads = t.world.count("launchAccounts");
    await t.tick();
    expect(t.world.count("launchAccounts")).toBe(reads);
    expect(t.world.count("launchAccountRows")).toBe(1);
  });

  it("lateUsageCannotRetargetAnotherComputer", async () => {
    const t = setup();
    t.world.hold("accounts:mac");
    await t.open({ agent: "codex", computer: "mac" });
    t.act.chooseComputer(t.state().computers.findIndex((c) => c.target === ""));
    await settle();
    expect(t.state().accounts.length).toBe(5);
    await t.choose("work");
    t.world.release("accounts:mac");
    await t.fire("mac");
    expect(t.state().accounts[t.state().account].tooltip).toContain("17% used");
    expect(t.state().accounts[t.state().account].tooltip).not.toContain("91% used");
    expect(t.world.count("launchAccountRows")).toBe(0);
  });

  it("selectedComputerCatalogRejectsLateOtherComputerRows", async () => {
    const t = setup((w) => w.flags.add("machine-accounts"));
    t.world.hold("accounts:mac");
    await t.open({ agent: "claude", computer: "mac" });
    expect(t.state().accounts.length).toBe(1);
    expect(t.state().account).toBe(-1);
    t.act.chooseComputer(t.state().computers.findIndex((c) => c.target === ""));
    await settle();
    expect(t.row("pc-claude")).toBe(0);
    expect(t.state().accounts.length).toBe(2);
    expect(t.state().accounts[0].name).toBe("pc@example.invalid");
    t.world.release("accounts:mac");
    await settle();
    expect(t.row("mac-claude")).toBe(-1);
    expect(t.state().accounts.length).toBe(2);
    t.act.chooseComputer(t.state().computers.findIndex((c) => c.target === "mac"));
    expect(t.row("pc-claude")).toBe(-1);
    await settle();
    expect(t.row("mac-claude")).toBe(0);
    expect(t.state().accounts[0].name).toBe("mac@example.invalid");
  });

  it("pendingAccountsAllowShellOnlyWhenChosen", async () => {
    const t = setup();
    t.world.hold("accounts:mac");
    await t.open({ agent: "claude", computer: "mac", path: "/workspace/example" });
    expect(t.state().account).toBe(-1);
    expect(t.state().form?.start_enabled).toBe(false);
    t.act.chooseAccount(0);
    await settle();
    expect(t.state().form?.start_enabled).toBe(true);
    await t.start();
    expect(t.choices()).toMatchObject({ host: "mac", agent: "sh" });
    t.world.release("accounts:mac");
  });

  for (const host of ["", "mac"]) {
    it(`pairsAndLimitsLaunchExactProfile (${host || "this computer"})`, async () => {
      const t = setup();
      await t.open({ agent: "codex", computer: host, path: "/workspace/example" });
      const rows = t.state().accounts;
      expect(rows.length).toBe(5);
      const work = rows[t.row("work")];
      expect(work.tooltip).toContain(host ? "91% used" : "17% used");
      expect(work.tooltip).toContain("5h");
      expect(work.tooltip).toContain("7d");
      expect(work.tooltip).not.toContain("Reset in");
      expect(work.name).toBe("shared@example.invalid");
      expect(rows[t.row("other")].name).toBe("shared@example.invalid");
      expect(work.tooltip.split("\n").length).toBe(2);
      expect(rows[t.state().account].id).toBe("other");
      expect(rows[t.row("native-dsh")].tooltip).toBe("shared@example.invalid\nlimits n/a");
      expect(rows[rows.length - 1].name).toBe("Run shell");
      await t.choose("work");
      await t.start();
      expect(t.choices()).toMatchObject({ host, agent: "codex", account: "work", machine: host || "pc" });
    });
  }

  // Opening asks for limits; their replies change rows in place: rows, choice and order stay.
  it("openingRefreshesLimitsInPlace", async () => {
    const t = setup((w) => w.flags.add("missing"));
    await t.open({ agent: "claude" });
    expect(t.world.of("launchAccounts").map((c) => c.args)).toEqual([["", false, "claude"]]);
    expect(t.state().accounts[t.row("work")].tooltip).toBe("shared@example.invalid\nlimits n/a");
    await t.choose("claude");
    const keys = t.state().accounts.map((r) => r.key);
    t.world.flags.delete("missing");
    await t.fire("");
    expect(t.state().accounts.map((r) => r.key)).toEqual(keys);
    expect(t.state().accounts[t.state().account].id).toBe("claude");
    for (const id of ["work", "claude"]) expect(t.state().accounts[t.row(id)].tooltip).toContain("17% used");
    // Another computer's limits change nothing here; the minute timer only repaints.
    await t.fire("mac");
    await t.tick();
    expect(t.world.count("launchAccounts")).toBe(1);
    expect(t.world.of("launchAccountRows").map((c) => c.args)).toEqual([[""], [""]]);
    // Refresh asks again at once, with forced limits.
    t.act.refreshAccounts();
    await settle();
    expect(t.world.of("launchAccounts").at(-1)?.args).toEqual(["", true, "claude"]);
  });

  it("unknownAndEndedWindowsStayUnavailable", async () => {
    const t = setup((w) => w.flags.add("missing"));
    await t.open({ agent: "codex" });
    expect(t.state().accounts[t.row("work")].tooltip).toBe("shared@example.invalid\nlimits n/a");
    t.world.flags.delete("missing");
    t.world.flags.add("ended");
    t.act.refreshAccounts();
    await settle();
    expect(t.state().accounts[t.row("work")].tooltip).toBe("shared@example.invalid\nlimits n/a");
  });

  // 944-950: while the computer's catalog is not filed, the list keeps loading.
  it("keeps loading while the core answers before the catalog is filed", async () => {
    const t = setup((w) => w.unfiled.add("mac"));
    await t.open({ agent: "claude", computer: "mac", path: "/workspace/example" });
    expect(t.world.of("launchAccounts").map((c) => c.args)).toEqual([["mac", false, "claude"]]);
    expect(t.state()).toMatchObject({ account: -1, accountStatus: "Loading accounts…", refreshEnabled: false });
    expect(t.state().accounts.map((r) => r.shell)).toEqual([true]);
    expect(t.state().form?.start_enabled).toBe(false);
    // The reply is filed: its event brings the catalog, as Qt's accountsReady.
    t.world.unfiled.delete("mac");
    await t.fire("mac");
    expect(t.state().accounts[t.state().account].id).toBe("claude");
    expect(t.state()).toMatchObject({ accountStatus: "", refreshEnabled: true });
    expect(t.state().form?.start_enabled).toBe(true);
    expect(t.world.count("launchAccounts")).toBe(1);
  });

  it("takes a catalog filed before the core's late answer arrived", async () => {
    const t = setup((w) => w.unfiled.add(""));
    t.world.hold("accounts:");
    await t.open({ agent: "codex" });
    // The reply is filed, and its event sent, while the core's empty answer is on its way.
    t.world.unfiled.delete("");
    await t.fire("");
    expect(t.state().account).toBe(-1);
    t.world.release("accounts:");
    await settle();
    expect(t.state().accounts[t.state().account].id).toBe("other");
    expect(t.state().accountStatus).toBe("");
    expect(t.world.count("launchAccounts")).toBe(1);
  });

  // loadAccounts reads the selected computer itself (865-874): one hgs cannot read shows its error, never a wait.
  it("a computer hgs cannot read shows its error and offers the shell", async () => {
    const t = setup((w) => w.unreadable.set("lab", "Could not load accounts: hgs: Unknown machine lab."));
    await t.open({ agent: "claude", computer: "lab", path: "/workspace/example" });
    expect(t.state().computers.find((c) => c.target === "lab")?.label).toBe("lab (offline)");
    expect(t.state()).toMatchObject({ accountStatus: "Could not load accounts: hgs: Unknown machine lab.", refreshEnabled: true, account: 0 });
    expect(t.state().accounts.map((r) => r.name)).toEqual(["Run shell"]);
    expect(t.world.count("launchAccountRows")).toBe(0);
  });

  it("drops a limits answer older than the newest", async () => {
    const t = setup((w) => w.flags.add("missing"));
    await t.open({ agent: "codex" });
    t.world.hold("rows:1");
    await t.fire("");
    t.world.flags.delete("missing");
    await t.fire("");
    expect(t.state().accounts[t.row("work")].tooltip).toContain("17% used");
    t.world.release("rows:1");
    await settle();
    expect(t.state().accounts[t.row("work")].tooltip).toContain("17% used");
  });

  it("closing ends the dialog's account watch", async () => {
    const t = setup();
    await t.open();
    t.store.closeNewSession();
    await settle();
    expect(t.store.newSession()).toBeUndefined();
    expect(t.world.count("launchAccountsClose")).toBe(1);
    expect(t.world.listeners.size).toBe(0);
    expect(t.world.ticks.length).toBe(0);
  });
});

describe("repository source (NewSessionDialog.cpp 590-639)", () => {
  for (const host of ["", "mac"]) {
    for (const override of ["", "existing", "ungrouped"]) {
      it(`repositorySourceKeepsNewProjectIntent (${host || "local"}, ${override || "keep new"})`, async () => {
        const t = setup((w) => { w.projects = [project("ungrouped", "Inbox", [], { is_default: true }), project("existing", "Existing")]; });
        await t.open({ agent: "codex", project: "existing", computer: host });
        t.act.setSource(true);
        t.act.setUrl("git@example.invalid:team/fresh.git");
        await settle();
        t.act.chooseProject(t.state().projects.length - 1);
        await settle();
        expect(t.state().newProjectVisible).toBe(true);
        t.act.setNewProjectName("My new project");
        t.act.setSource(false);
        await settle();
        expect(t.state().newProjectVisible).toBe(false);
        expect(t.state().projects[t.state().project].id).not.toBe("");
        // A repeated source click must not erase the remembered repository choice.
        t.act.setSource(false);
        if (override) t.act.chooseProject(t.state().projects.findIndex((p) => p.id === override));
        t.act.setSource(true);
        await settle();
        expect(t.state().newProjectVisible).toBe(!override);
        expect(t.state().newProjectName).toBe("My new project");
        expect(t.state().projects[t.state().project].id).toBe(override);
        expect(t.world.creating()).toEqual([]);
      });
    }
  }

  it("repositorySourceKeepsOpaqueProjectId", async () => {
    const id = "new-repository-project";
    const t = setup((w) => { w.projects = [project(id, "Existing project", [], { is_default: true })]; });
    await t.open({ agent: "codex", project: id });
    t.act.setSource(true);
    t.act.setUrl("git@example.invalid:team/fresh.git");
    t.act.setDestination("/workspace/fresh");
    await settle();
    expect(t.state().newProjectVisible).toBe(false);
    expect(t.state().form?.start_enabled).toBe(true);
    await t.start();
    expect(t.choices()).toMatchObject({ project: id, repository: { url: "git@example.invalid:team/fresh.git", destination: "/workspace/fresh", new_project: false } });
  });

  // restoreCreationChoices 792-799: the recipe's project name comes last, after its destination.
  it("Edit choices… keeps a repository's own new project name", async () => {
    const t = setup((w) => { w.projects = [project("ungrouped", "Inbox", [], { is_default: true }), project("existing", "Existing")]; });
    const recipe = { id: "failed-1", tag: "retry", account: "work", repository: true, url: "git@example.invalid:team/fresh.git",
      target: "/workspace/fresh", new_project: true, project_name: "Team project" };
    await t.open({ agent: "codex", recipe });
    expect(t.state()).toMatchObject({ repository: true, destination: "/workspace/fresh", newProjectVisible: true, newProjectName: "Team project" });
    await t.start();
    expect(t.choices()).toMatchObject({ predecessor: "failed-1", repository: { destination: "/workspace/fresh", name: "fresh", new_project: true, project_name: "Team project" } });
  });

  it("a later edit of the restored destination names the project again", async () => {
    const t = setup();
    await t.open({ agent: "codex", recipe: { repository: true, url: "git@example.invalid:team/fresh.git", target: "/workspace/fresh", new_project: true, project_name: "Team project" } });
    t.act.setDestination("/workspace/renamed");
    await settle();
    expect(t.state().newProjectName).toBe("renamed");
  });

  it("the URL names the new project until its name is edited, at most 80 characters", async () => {
    const t = setup();
    await t.open({ agent: "codex" });
    t.act.setSource(true);
    await settle();
    expect(t.world.of("repositoryFields")[0].args).toEqual(["", "", "posix", false, null]);
    expect(t.state().focusUrl).toBe(1);
    t.act.setUrl("https://github.com/example/first.git");
    await settle();
    expect(t.state().destination).toBe("~/first");
    expect(t.state().newProjectName).toBe("first");
    t.act.setDestination("/workspace/custom");
    t.act.setUrl("git@example.invalid:team/third.git");
    await settle();
    expect(t.state().destination).toBe("/workspace/custom");
    t.act.setNewProjectName("x".repeat(90));
    t.act.setDestination("/workspace/renamed");
    await settle();
    expect(t.state().newProjectName).toBe("x".repeat(80));
    // Another computer starts fresh fields with the URL and the old name.
    t.act.chooseComputer(t.state().computers.findIndex((c) => c.target === "mac"));
    await settle();
    expect(t.world.of("repositoryFields").at(-1)?.args).toEqual(["git@example.invalid:team/third.git", "", "posix", false, "renamed"]);
    expect(t.state().destination).toBe("~/renamed");
  });

  it("repositorySourceIgnoresLateFolderPrefill", async () => {
    const t = setup((w) => {
      w.flags.add("prefill-linked");
      w.projects = [project("example", "Example", [folder("f1", "pc", "/workspace/example")], { is_default: true })];
      w.selection = { row: { computer: "", cwdPath: "/workspace/example-work-one", cwdCanonical: "", project: "Example" } as Session };
    });
    t.world.hold("prefill");
    await t.open({ agent: "codex" });
    expect(t.state().form?.preview?.source).toBe("Checking the session's main working folder…");
    t.act.setSource(true);
    t.act.setUrl("git@example.invalid:team/fresh.git");
    t.act.setDestination("/workspace/keep-name");
    t.world.release("prefill");
    await settle();
    const linked = t.world.of("worktreeCatalog").filter((c) => c.args[1] === "/workspace/example-work-one");
    expect(linked.map((c) => c.args[2])).toEqual([false, true]);
    expect(t.state().folderPath).toBe("/workspace/example-work-one");
    expect(t.state().destination).toBe("/workspace/keep-name");
    t.act.setSource(false);
    await settle();
    expect(t.state().folderPath).toBe("/workspace/example-work-one");
    expect(t.world.creating()).toEqual([]);
  });

  it("sourceSwitchKeepsControls", async () => {
    const t = setup((w) => { w.projects = [project("example", "Example", [folder("f1", "pc", "/workspace/example")], { is_default: true })]; });
    await t.open({ agent: "codex", project: "example" });
    t.act.setAuto(false);
    t.act.setName("keep-this-session");
    for (let i = 0; i < 4; i++) { t.act.setSource(!t.state().repository); await settle(); }
    expect(t.state()).toMatchObject({ repository: false, autoPreferred: false, name: "keep-this-session" });
    expect(t.state().projects[t.state().project].id).toBe("example");
    expect(t.state().folderPath).toBe("/workspace/example");
    expect(t.world.creating()).toEqual([]);
  });
});

describe("placement", () => {
  it("untouchedInboxUsesNativeFolderInferenceAndExplicitInboxWins", async () => {
    for (const explicit of [0, 1, 2]) {
      const t = setup((w) => { w.swarm = "swarm-fixture"; });
      await t.open({ agent: "codex", path: "/workspace/example", ...(explicit === 1 ? { project: "ungrouped" } : {}) });
      if (explicit === 2) t.act.chooseProject(t.state().project);
      await settle();
      await t.start();
      expect(t.choices()).toMatchObject({ project: "ungrouped", project_name: "Inbox", implicit_inbox: explicit === 0, swarm: "swarm-fixture", listed_folder: false });
    }
  });
});

describe("Start (NewSessionDialog.cpp 743-786)", () => {
  const windowsCard = machineMeta("pc", {}, { platform: "windows", local_host: { state: "running", pid: 4242, protocol: 1, conpty: "bundled", integrity: "medium", windows_session: 2, error: null } });

  it("launchIntoAnotherWindowsSessionAsksFirst", async () => {
    const t = setup((w) => { w.cards[""] = windowsCard; w.warning = "The session host on this computer runs in another Windows session (2), for example one started over SSH. Launch anyway?"; });
    await t.open({ agent: "codex", path: "C:/work/example" });
    await t.choose("work");
    t.act.setName("keep-this-name");
    await settle();
    expect(t.state().form?.start_enabled).toBe(true);
    // Cancel keeps the form as it was: nothing is checked, created or started.
    const asked = await t.start(false);
    expect(asked.length).toBe(1);
    expect(asked[0]).toContain("another Windows session (2)");
    expect(t.world.of("launchWarning")[0].args).toEqual([windowsCard, ""]);
    expect(t.store.newSession()).toBeDefined();
    expect(t.world.count("newSessionPlan")).toBe(0);
    expect(t.world.creating()).toEqual([]);
    expect(t.state()).toMatchObject({ name: "keep-this-name", submitted: false });
    expect(t.state().accounts[t.state().account].id).toBe("work");
    expect(t.state().folderPath).toBe("C:\\work\\example");
    // Launch anyway submits exactly once, after a single question.
    const again = await t.start(true);
    expect(again.length).toBe(1);
    expect(t.world.count("creationSubmit")).toBe(1);
    expect(t.choices()).toMatchObject({ host: "", agent: "codex", path: "C:/work/example", name: "keep-this-name", account: "work" });
    expect(t.store.newSession()).toBeUndefined();
    expect(t.world.admitted).toEqual(["creation-1"]);
  });

  it("asks Start twice before it submits, and sends no creating command before", async () => {
    const t = setup();
    await t.open({ agent: "codex", path: "/workspace/example" });
    t.act.setName("at-the-click");
    await settle();
    // A change while Start's last question is under way is not the click's.
    t.world.onStart = (n) => { if (n === 2) { t.act.setName("after-the-click"); t.act.chooseAccount(t.row("work")); } };
    const before = t.world.calls.length;
    await t.start();
    const sent = t.world.calls.slice(before).map((c) => c.command).filter((c) => c !== "newSessionForm");
    expect(sent).toEqual(["newSessionStart", "launchWarning", "newSessionStart", "newSessionPlan", "creationSubmit", "launchAccountsClose"]);
    // The plan and Start read the same input, and the choices of the same turn.
    const [startInput] = t.world.of("newSessionStart")[1].args as [NewSessionFormInput];
    const [planInput, choices] = t.world.of("newSessionPlan")[0].args as [NewSessionFormInput, Json];
    expect(planInput).toEqual(startInput);
    expect(choices).toMatchObject({ name: "at-the-click", account: "other", agent: startInput.form.agent, path: startInput.form.path });
  });

  it("two Start clicks submit once", async () => {
    const t = setup();
    await t.open({ agent: "codex", path: "/workspace/example" });
    let answer!: (ok: boolean) => void;
    t.world.warning = "Launch anyway?";
    const first = t.store.startNewSession(() => new Promise((resolve) => { answer = resolve; }));
    await settle();
    const second = t.store.startNewSession(async () => true);
    answer(true);
    await Promise.all([first, second]);
    await settle();
    expect(t.world.count("creationSubmit")).toBe(1);
    expect(t.world.count("launchWarning")).toBe(1);
  });

  it("a refused admission keeps every choice and shows the reason", async () => {
    const t = setup((w) => { w.admission = { refused: "Too many creations are waiting. Let one finish first." }; });
    await t.open({ agent: "codex", path: "/workspace/example" });
    await t.choose("work");
    t.act.setName("keep-me");
    await settle();
    await t.start();
    expect(t.store.newSession()).toBeDefined();
    expect(t.state()).toMatchObject({ error: "Too many creations are waiting. Let one finish first.", submitted: false, name: "keep-me", folderPath: "/workspace/example" });
    expect(t.state().accounts[t.state().account].id).toBe("work");
    expect(t.world.admitted).toEqual([]);
    // The next Start submits again: only a refusal clears the submission.
    t.world.admission = { admitted: "creation-2" };
    await t.start();
    expect(t.world.count("creationSubmit")).toBe(2);
    expect(t.world.admitted).toEqual(["creation-2"]);
  });

  // The contract Task 5 implements: a refusal clears the submission; a lost call is an unknown outcome.
  it("a lost submission keeps it submitted and is never sent again", async () => {
    const t = setup((w) => { w.admission = new Error("hgs did not answer"); });
    await t.open({ agent: "codex", path: "/workspace/example" });
    await t.start();
    expect(t.store.newSession()).toBeDefined();
    expect(t.state()).toMatchObject({ error: "hgs did not answer", submitted: true });
    t.world.admission = { admitted: "creation-2" };
    await t.start();
    expect(t.world.count("creationSubmit")).toBe(1);
    expect(t.world.admitted).toEqual([]);
  });

  it("an admission after the dialog closed still reaches the creations", async () => {
    const t = setup();
    await t.open({ agent: "codex", path: "/workspace/example" });
    t.world.hold("submit");
    const started = t.store.startNewSession(async () => true);
    await settle();
    expect(t.world.count("creationSubmit")).toBe(1);
    t.store.closeNewSession();
    t.world.release("submit");
    await started;
    expect(t.world.admitted).toEqual(["creation-1"]);
    expect(t.store.newSession()).toBeUndefined();
  });

  it("carries the computer's identity and the folder's project row", async () => {
    const t = setup((w) => {
      w.cards.mac = machineMeta("mac-mini.example.invalid");
      w.projects = [project("work", "Work", [folder("f1", "mac", "/srv/repo")], { is_default: true })];
    });
    await t.open({ agent: "codex", project: "work", computer: "mac", folder: "f1", recipe: { id: "failed-1", tag: "retry", account: "work", source: "/srv/repo", target: "/srv/repo", auto: false } });
    await settle();
    await t.start();
    expect(t.choices()).toMatchObject({ host: "mac", machine: "mac-mini.example.invalid", project: "work", project_name: "Work", listed_folder: true, path: "/srv/repo", predecessor: "failed-1", name: "retry", account: "work" });
  });
});

describe("opening (SessionsWindow::showNewSession 2895-2928)", () => {
  const row = (extra: Partial<Session>) => ({ key: "mac\nrow-1", computer: "mac", cwdPath: "/srv/session", cwdCanonical: "/srv/canonical", project: "Docs", ...extra } as Session);

  it("takes the selected row's project from the organization, else the default; a project option wins and makes it explicit", async () => {
    const projects = [project("ungrouped", "Inbox", [], { is_default: true }), project("work", "Work"), project("docs", "Docs")];
    // The row's catalog project name is not its organization project: groupOf is (Qt's groupFor).
    const t = setup((w) => { w.projects = projects; w.selection = { row: row({}) }; w.groups = { "mac\nrow-1": "work" }; });
    await t.open();
    expect(t.state().projects[t.state().project].id).toBe("work");
    t.store.closeNewSession();
    t.world.groups = {};
    await t.open();
    expect(t.state().projects[t.state().project].id).toBe("ungrouped");
    t.store.closeNewSession();
    t.world.selection = undefined;
    await t.open();
    expect(t.state().projects[t.state().project].id).toBe("ungrouped");
    t.store.closeNewSession();
    await t.open({ project: "Docs", path: "/srv/x" });
    expect(t.state().projects[t.state().project].id).toBe("docs");
    await t.start();
    expect(t.choices()).toMatchObject({ project: "docs", implicit_inbox: false });
  });

  it("takes the selected creation's project without making it explicit", async () => {
    const t = setup((w) => {
      w.projects = [project("ungrouped", "Inbox", [], { is_default: true }), project("work", "Work")];
      w.creationProject = "work";
    });
    await t.open({ path: "/workspace/example" });
    expect(t.state().projects[t.state().project].id).toBe("work");
    t.store.closeNewSession();
    // An Inbox card still uses native folder inference, as Qt's non-explicit project does.
    t.world.creationProject = "ungrouped";
    await t.open({ path: "/workspace/example" });
    expect(t.state().projects[t.state().project].id).toBe("ungrouped");
    await t.start();
    expect(t.choices()).toMatchObject({ project: "ungrouped", implicit_inbox: true });
  });

  it("uses the selected session as context only for a generic open", async () => {
    const t = setup((w) => {
      w.projects = [project("work", "Work", [folder("f1", "mac", "/srv")], { is_default: true })];
      w.selection = { row: row({}), details: { cwd: "/srv/details", cwd_canonical: null } as Details };
    });
    await t.open();
    expect(t.state().host).toBe("mac");
    // The folder is the details' cwd, matched to the project folder holding it.
    expect(t.state().folderPath).toBe("/srv");
    expect(t.world.of("worktreeCatalog").find((c) => c.args[2] === true)?.args).toEqual(["mac", "/srv/details", true]);
    t.store.closeNewSession();
    await t.open({ path: "/srv/other" });
    expect(t.world.of("worktreeCatalog").filter((c) => c.args[2] === true).length).toBe(1);
    expect(t.state().host).toBe("");
  });

  it("a changed project folder raises a sticky notice and opens nothing", async () => {
    const t = setup((w) => { w.projects = [project("work", "Work", [folder("f1", "pc", "/srv/a")], { is_default: true })]; });
    await t.open({ project: "work", folder: "gone" });
    expect(t.store.newSession()).toBeUndefined();
    expect(t.world.notices).toEqual([{ tone: "error", text: "This project folder changed. Select it again.", sticky: true }]);
    expect(t.world.count("launchAccountsClose")).toBe(1);
    await t.open({ project: "work", folder: "f1" });
    expect(t.state().folderPath).toBe("/srv/a");
    expect(t.state().folders[t.state().folder].key).toBe("f1");
  });

  it("a failure while opening disposes the dialog it made", async () => {
    const t = setup();
    const recipe = Object.defineProperty({}, "tag", { enumerable: true, get: () => { throw new Error("unreadable recipe"); } });
    await t.open({ agent: "codex", recipe });
    expect(t.store.newSession()).toBeUndefined();
    expect(t.world.notices).toEqual([{ tone: "error", text: "unreadable recipe", sticky: true }]);
    expect(t.world.count("launchAccountsClose")).toBe(1);
    expect(t.world.listeners.size).toBe(0);
    expect(t.world.ticks.length).toBe(0);
    await t.open();
    expect(t.store.newSession()).toBeDefined();
  });

  it("opens one dialog at a time and starts from Qt's defaults", async () => {
    const t = setup();
    await t.open();
    await t.open({ agent: "claude" });
    expect(t.world.count("newSessionContext")).toBe(1);
    expect(t.state()).toMatchObject({ name: "work-1a2b3c4d", openTerminal: false, autoPreferred: true, agent: "codex" });
  });

  // setFleet (478-486) runs for every fleet change the form reads, the session host included (launch.rs 178-185).
  it("the fleet key changes with this computer's session host", () => {
    const card = machineMeta("pc", {}, { platform: "windows", local_sessions: false });
    const key = (local: MachineMeta) => fleetKey({ "": local, mac: machineMeta("mac") }, ["mac"], () => true);
    const before = key(card);
    expect(key({ ...card })).toBe(before);
    expect(key({ ...card, local_host: { state: "stopped", pid: null, protocol: 1, conpty: "bundled", integrity: "medium", windows_session: null, error: null } })).not.toBe(before);
    expect(key({ ...card, local_sessions: true })).not.toBe(before);
    expect(fleetKey({ "": card }, ["mac"], (c) => c !== "mac")).not.toBe(fleetKey({ "": card }, ["mac"], () => true));
  });

  it("selectedHostTarget names a single peer filter only", () => {
    const t = setup();
    expect(t.store.selectedHostTarget()).toBe("");
    t.world.filters = ["mac"];
    expect(t.store.selectedHostTarget()).toBe("mac");
    t.world.filters = [""];
    expect(t.store.selectedHostTarget()).toBe("");
    t.world.filters = ["mac", "studio"];
    expect(t.store.selectedHostTarget()).toBe("");
  });

  it("Manage this project's folders and Open Machines close first", async () => {
    const t = setup((w) => { w.projects = [project("work", "Work", [], { is_default: true })]; });
    await t.open();
    t.act.manageFolders();
    expect(t.store.newSession()).toBeUndefined();
    await t.open();
    t.act.openMachines();
    expect(t.store.newSession()).toBeUndefined();
    expect(t.world.opened).toEqual(["projects:work", "machines"]);
  });
});

describe("the organization's projects and folders (SessionsWindow.cpp:2923, NewSessionDialog.cpp:512-575)", () => {
  const world = (w: World) => {
    w.projects = [
      project("work", "Work", [folder("f1", "pc", "/srv/work"), folder("f2", "lab", "/srv/lab")], { is_default: true }),
      project("hidden", "Hidden", [], { accessible: false }),
      project("docs", "Docs", [folder("", "pc", "/srv/docs")]),
    ];
  };

  it("lists the accessible projects in the organization's order", async () => {
    const t = setup(world);
    await t.open();
    expect(t.state().projects.map((p) => p.id)).toEqual(["work", "docs"]);
  });

  it("copies a project's folders once, at open and on its first choice, and keeps the copy", async () => {
    const t = setup(world);
    await t.open();
    expect(t.world.of("projectFolders").map((c) => c.args)).toEqual([["work"]]);
    expect(t.state().folders.map((f) => f.path)).toEqual(["/srv/work"]);
    expect(t.state().computers.map((c) => c.target)).toEqual(["", "mac", "lab"]);
    t.world.projects[0].folders = [folder("f9", "pc", "/srv/changed")];
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "docs"));
    await settle();
    expect(t.state().folders.map((f) => [f.key, f.path])).toEqual([["pc\n/srv/docs", "/srv/docs"]]);
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "work"));
    await settle();
    // Qt's copy at open: a later organization change does not reach this dialog.
    expect(t.state().folders.map((f) => f.path)).toEqual(["/srv/work"]);
    expect(t.world.of("projectFolders").map((c) => c.args)).toEqual([["work"], ["docs"]]);
    expect(t.world.creating()).toEqual([]);
  });

  it("shows a project without folders until its copy arrives, then refills", async () => {
    const t = setup(world);
    await t.open();
    t.world.hold("folders:docs");
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "docs"));
    await settle();
    expect(t.state().folders).toEqual([]);
    expect(t.state().computers.map((c) => c.target)).toEqual(["", "mac"]);
    t.world.release("folders:docs");
    await settle();
    expect(t.state().folders.map((f) => f.path)).toEqual(["/srv/docs"]);
    expect(t.state().folder).toBe(0);
  });

  it("keeps a late copy for a project no longer chosen, and it changes nothing", async () => {
    const t = setup(world);
    await t.open();
    t.world.hold("folders:docs");
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "docs"));
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "work"));
    await settle();
    const before = t.state();
    t.world.release("folders:docs");
    await settle();
    expect(t.state().folders).toEqual(before.folders);
    expect(t.state().computers).toEqual(before.computers);
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "docs"));
    // The kept copy answers at once.
    expect(t.state().folders.map((f) => f.path)).toEqual(["/srv/docs"]);
    expect(t.world.count("projectFolders")).toBe(2);
  });

  it("opens only once the chosen project's folders are copied", async () => {
    const t = setup(world);
    t.world.hold("folders:work");
    const opening = t.store.openNewSession({ project: "work", folder: "f1" });
    await settle();
    expect(t.store.newSession()).toBeUndefined();
    t.world.release("folders:work");
    await opening;
    await settle();
    expect(t.state().folders[t.state().folder]).toMatchObject({ key: "f1", path: "/srv/work" });
    expect(t.world.notices).toEqual([]);
  });

  // A lost project_folders call is never a project without folders (Task 3's re-review).
  it("shows a failed folder copy on the error line and reads it again when the project is chosen again", async () => {
    const t = setup((w) => { world(w); w.failFolders = (id, call) => (id === "work" && call === 1 ? "The app did not answer." : undefined); });
    await t.open();
    expect(t.state().error).toBe("The app did not answer.");
    expect(t.state().folders).toEqual([]);
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "docs"));
    await settle();
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "work"));
    await settle();
    expect(t.world.of("projectFolders").map((c) => c.args)).toEqual([["work"], ["docs"], ["work"]]);
    expect(t.state().folders.map((f) => f.path)).toEqual(["/srv/work"]);
    expect(t.state().error).toBe("");
    expect(t.world.creating()).toEqual([]);
  });

  it("a folder entry whose copy fails shows the call's text, not a changed folder", async () => {
    const t = setup((w) => { world(w); w.failFolders = (id, call) => (id === "work" && call === 2 ? "The app did not answer." : undefined); });
    // Projects' entry checks the folder first (the first call), then the dialog copies it (the second).
    expect(await t.store.openFolderSession("work", folder("f1", "pc", "/srv/work"))).toBe("");
    await settle();
    expect(t.store.newSession()).toBeUndefined();
    expect(t.world.notices).toEqual([{ tone: "error", text: "The app did not answer.", sticky: true }]);
    expect(t.world.count("launchAccountsClose")).toBe(1);
    // A failed first call is the entry's own problem.
    t.world.failFolders = () => "Lost call";
    expect(await t.store.openFolderSession("work", folder("f1", "pc", "/srv/work"))).toBe("Lost call");
  });

  // A folder without an id is selected by its key (its computer and path); its id stays empty.
  it("selects a project folder by its key", async () => {
    const t = setup(world);
    await t.open({ project: "docs", folder: "pc\n/srv/docs" });
    expect(t.state().folders[t.state().folder].key).toBe("pc\n/srv/docs");
    await t.start();
    expect(t.choices()).toMatchObject({ project: "docs", listed_folder: true, path: "/srv/docs" });
    t.store.closeNewSession();
    expect(await t.store.openFolderSession("docs", folder("", "pc", "/srv/docs"))).toBe("");
    await settle();
    expect(t.state().folders[t.state().folder].key).toBe("pc\n/srv/docs");
  });
});

describe("the Projects entries (ProjectsDialog.cpp 523-541)", () => {
  it("checks the folder is still the project's and can start a session", async () => {
    const f1 = folder("f1", "pc", "/srv/a"), remote = folder("f2", "mac", "/srv/b");
    const t = setup((w) => { w.projects = [project("work", "Work", [f1, remote], { is_default: true })]; });
    expect(await t.store.openFolderSession("work", { ...f1, path: "/srv/moved" })).toBe("This project folder changed. Select it again.");
    t.world.localExists = false;
    expect(await t.store.openFolderSession("work", f1)).toBe("This folder is unavailable on this computer.");
    t.world.online.mac = false;
    expect(await t.store.openFolderSession("work", remote)).toBe("Reconnect mac to start a session in this folder.");
    t.world.cards[""] = machineMeta("");
    expect(await t.store.openFolderSession("work", f1)).toBe("Waiting for this computer's identity.");
    expect(t.store.newSession()).toBeUndefined();
    t.world.cards[""] = machineMeta("pc");
    t.world.localExists = true;
    expect(await t.store.openFolderSession("work", f1)).toBe("");
    await settle();
    expect(t.state().folders[t.state().folder].key).toBe("f1");
    expect(t.world.of("localFolderExists").map((c) => c.args)).toEqual([["/srv/a"], ["/srv/a"]]);
  });
});

describe("reads only (Review Focus 1 and 2)", () => {
  it("opening, choosing, browsing and Cancel send only reads", async () => {
    const t = setup((w) => {
      w.flags.add("opencode");
      w.flags.add("opencode-models");
      w.flags.add("opencode-stopped");
      w.projects = [project("work", "Work", [folder("f1", "pc", "/workspace/example")], { is_default: true })];
      w.picked = "/workspace/picked";
    });
    await t.open({ agent: "opencode" });
    t.act.chooseComputer(t.state().computers.findIndex((c) => c.target === "mac"));
    await settle();
    t.act.chooseComputer(t.state().computers.findIndex((c) => c.target === ""));
    await settle();
    await t.act.browseLocal();
    await settle();
    expect(t.state().folderPath).toBe("/workspace/picked");
    t.act.openModelPicker();
    await settle();
    t.act.pickerToggleAll();
    await settle();
    t.act.setSource(true);
    t.act.setUrl("git@example.invalid:team/fresh.git");
    t.act.setSource(false);
    t.act.setAuto(false);
    t.act.setAuto(true);
    t.act.refreshAccounts();
    await settle();
    t.store.closeNewSession();
    await settle();
    expect(t.world.creating()).toEqual([]);
    const commands = new Set(t.world.calls.map((c) => c.command));
    for (const command of commands) expect(["newSessionContext", "projectFolders", "newSessionForm", "repositoryFields", "worktreeSnapshot", "worktreeCatalog", "pickLocalFolder", "openCodeModels", "launchAccounts", "launchAccountsClose"]).toContain(command);
  });

  it("the prefill's late answer after a folder change does nothing", async () => {
    const t = setup((w) => {
      w.flags.add("prefill-linked");
      w.projects = [project("example", "Example", [folder("f1", "pc", "/workspace/example")], { is_default: true })];
      w.selection = { row: { computer: "", cwdPath: "/workspace/example-work-one", cwdCanonical: "", project: "Example" } as Session };
    });
    t.world.hold("prefill");
    await t.open({ agent: "codex" });
    expect(t.state().folderPath).toBe("/workspace/example-work-one");
    t.act.selectPath("/workspace/elsewhere", false);
    t.world.release("prefill");
    await settle();
    expect(t.state().folderPath).toBe("/workspace/elsewhere");
    expect(t.state().form?.preview?.source).not.toBe("Checking the session's main working folder…");
  });

  it("the prefill moves a linked worktree's session to its main checkout", async () => {
    const t = setup((w) => {
      w.flags.add("prefill-linked");
      w.projects = [project("example", "Example", [folder("f1", "pc", "/workspace/example")], { is_default: true })];
      w.selection = { row: { computer: "", cwdPath: "/workspace/example-work-one", cwdCanonical: "", project: "Example" } as Session };
    });
    await t.open({ agent: "codex" });
    expect(t.state().folders[t.state().folder].key).toBe("f1");
    expect(t.state().folderPath).toBe("/workspace/example");
    // The verified catalog travels with the form for that folder.
    const input = t.world.of("newSessionForm").at(-1)?.args[0] as NewSessionFormInput;
    expect(input.verified).toMatchObject({ selected_root: "/workspace/example-work-one" });
  });
});

// The child dialogs answer with the computer and project they opened with
// (browseFolder 581-588, chooseWorktree 1114-1124, createWorktree 1125-1143).
describe("the child dialogs' answers (Task 4)", () => {
  const world = (w: World) => {
    w.projects = [
      project("work", "Work", [folder("f1", "pc", "/srv/work"), folder("f2", "mac", "/srv/peer")], { is_default: true }),
      project("docs", "Docs", [folder("f3", "pc", "/srv/docs")]),
    ];
  };

  it("a browsed folder applies only while its computer and project are the dialog's", async () => {
    const t = setup(world);
    await t.open({ project: "work", computer: "mac" });
    expect(t.state()).toMatchObject({ host: "mac", style: "posix" });
    t.act.applyBrowsedFolder("mac", "work", "/home/example/notes/");
    expect(t.state()).toMatchObject({ folderPath: "/home/example/notes", autoPreferred: true });
    t.act.applyBrowsedFolder("", "work", "/srv/elsewhere");
    t.act.applyBrowsedFolder("mac", "docs", "/srv/elsewhere");
    expect(t.state().folderPath).toBe("/home/example/notes");
    // The clone's parent on a peer: the store builds the Clone to field from it.
    t.act.setSource(true);
    t.act.setUrl("https://example.invalid/team/app.git");
    await settle();
    t.act.setRepositoryParent("/home/example/src");
    await settle();
    expect(t.state().destination).toBe("/home/example/src/app");
    expect(t.world.creating()).toEqual([]);
  });

  it("Choose existing… sets None and selects the folder, only in its own context", async () => {
    const t = setup(world);
    await t.open({ project: "work", folder: "f1" });
    expect(t.state().autoPreferred).toBe(true);
    t.act.chooseExistingWorktree("mac", "work", "/srv/work-fix");
    t.act.chooseExistingWorktree("", "docs", "/srv/work-fix");
    expect(t.state()).toMatchObject({ folderPath: "/srv/work", autoPreferred: true });
    t.act.chooseExistingWorktree("", "work", "/srv/work-fix");
    expect(t.state()).toMatchObject({ folderPath: "/srv/work-fix", autoPreferred: false });
    expect(t.world.creating()).toEqual([]);
  });

  it("a created worktree joins as verified and is selected with None; in another context the error line names it", async () => {
    const t = setup(world);
    await t.open({ project: "work", folder: "f1" });
    const verified = { state: "ok", common_dir: "/srv/work/.git", worktrees: [{ path: "/srv/work-new", kind: "linked", available: true }] };
    t.act.worktreeCreated("", "work", "/srv/work-new", verified);
    await settle();
    expect(t.state()).toMatchObject({ folderPath: "/srv/work-new", autoPreferred: false });
    // The form reads that catalog as the new folder's verified one (1137-1141).
    const input = t.world.of("newSessionForm").at(-1)?.args[0] as NewSessionFormInput;
    expect(input.verified).toEqual(verified);
    t.act.chooseComputer(t.state().computers.findIndex((c) => c.target === "mac"));
    await settle();
    t.act.worktreeCreated("", "work", "/srv/work-other", verified);
    expect(t.state().error).toBe("Worktree created at /srv/work-other on pc. Select that folder to start a session.");
    expect(t.state().host).toBe("mac");
    t.act.worktreeCreated("mac", "docs", "/srv/peer-other", verified);
    expect(t.state().error).toBe("Worktree created at /srv/peer-other on mac. Select that folder to start a session.");
    // The store itself never creates: the dialog's own Create worktree did.
    expect(t.world.creating()).toEqual([]);
  });
});

describe("what the dialog view reads (Task 3)", () => {
  // updateForm 667: the New project row stays while the repository's New project… is remembered.
  it("keeps the New project row after leaving the repository source", async () => {
    const t = setup((w) => { w.projects = [project("ungrouped", "Inbox", [], { is_default: true }), project("existing", "Existing")]; });
    await t.open({ agent: "codex", project: "existing" });
    expect(t.state().newProjectRow).toBe(false);
    t.act.setSource(true);
    t.act.setUrl("git@example.invalid:team/fresh.git");
    await settle();
    t.act.chooseProject(t.state().projects.length - 1);
    await settle();
    expect(t.state()).toMatchObject({ newProjectVisible: true, newProjectRow: true });
    t.act.setSource(false);
    await settle();
    expect(t.state()).toMatchObject({ newProjectVisible: false, newProjectRow: true });
    // An explicit project in folder mode forgets the repository's choice (295).
    t.act.chooseProject(t.state().projects.findIndex((p) => p.id === "existing"));
    await settle();
    expect(t.state().newProjectRow).toBe(false);
  });

  // The repository's Browse on this computer (constructor 312-316): the parent folder, then its name below the choice.
  it("browses the clone's parent folder on this computer only", async () => {
    const t = setup((w) => { w.picked = "/workspace/parent"; });
    await t.open({ agent: "codex" });
    t.act.setSource(true);
    t.act.setUrl("git@example.invalid:team/fresh.git");
    await settle();
    expect(t.state().repositoryParent).toBe("~");
    await t.act.browseRepositoryLocal();
    await settle();
    expect(t.world.of("pickLocalFolder").map((c) => c.args)).toEqual([["~"]]);
    expect(t.state()).toMatchObject({ destination: "/workspace/parent/fresh", repositoryParent: "/workspace/parent" });
    t.act.chooseComputer(t.state().computers.findIndex((c) => c.target === "mac"));
    await settle();
    await t.act.browseRepositoryLocal();
    expect(t.world.count("pickLocalFolder")).toBe(1);
    expect(t.world.creating()).toEqual([]);
  });

  it("gives each computer row its online state", async () => {
    const t = setup((w) => { w.online.mac = false; });
    await t.open();
    expect(t.state().computers.map((c) => [c.target, c.online, c.label])).toEqual([["", true, "pc"], ["mac", false, "mac (offline)"]]);
  });

  // ProjectsDialog.cpp 515-532, 552-560: the entries' state without opening anything.
  it("tells the Projects entries why a folder cannot start a session, and its path", async () => {
    const f1 = folder("f1", "pc", "/srv/a"), remote = folder("f2", "mac", "/srv/b");
    const t = setup((w) => { w.projects = [project("work", "Work", [f1, remote], { is_default: true })]; });
    expect(t.store.folderLaunch(f1)).toEqual({ problem: "", shown: "/srv/a", local: true });
    expect(t.store.folderLaunch(f1, false)).toEqual({ problem: "This folder is unavailable on this computer.", shown: "/srv/a", local: true });
    expect(t.store.folderLaunch(remote)).toEqual({ problem: "", shown: "/srv/b", local: false });
    t.world.online.mac = false;
    expect(t.store.folderLaunch(remote).problem).toBe("Reconnect mac to start a session in this folder.");
    // Only this computer's folder is read; reading opens nothing.
    expect(await t.store.folderExists(remote)).toBeUndefined();
    t.world.localExists = false;
    expect(await t.store.folderExists(f1)).toBe(false);
    expect(t.world.of("localFolderExists").map((c) => c.args)).toEqual([["/srv/a"]]);
    expect(t.store.newSession()).toBeUndefined();
    expect(t.world.creating()).toEqual([]);
  });

  // The entries' state (folder-launch.ts) is the store's too: before this
  // computer's card reports a platform, its folders take the build style the
  // dialog's open learned, and the entries never ask it again.
  it("shows this computer's folders in the build style its open learned", async () => {
    const f1 = folder("f1", "pc", "C:/src/app");
    const t = setup((w) => {
      w.cards[""] = machineMeta("pc", {}, { platform: null });
      w.context = { branch: "work-1a2b3c4d", own_windows_session: null, build_style: "windows", remembered: { agent: "", accounts: {}, open_terminal: false }, swarm: "" };
      w.projects = [project("work", "Work", [f1], { is_default: true })];
    });
    expect(t.store.folderLaunch(f1).shown).toBe("C:/src/app");
    await t.open();
    expect(t.store.folderLaunch(f1)).toEqual({ problem: "", shown: "C:\\src\\app", local: true });
    expect(await t.store.folderExists(f1)).toBe(true);
    expect(t.world.count("newSessionContext")).toBe(1);
  });
});


describe("task executor admission", () => {
  const task = { project: "alpha", task: "11111111-1111-4111-8111-111111111111", stage_at: 123, assignment: { kind: "new_session", machine: "node-parent", session_id: "run_id:parent", name: "" }, target: { computer: "", name: "codex/example/parent", run_id: "parent", conversation_id: "conversation" }, catalog_key: "run:parent", session_id: "run_id:parent" };
  it("opening and cancelling reserves nothing; explicit Start carries the original task context once", async () => {
    const t = setup((w) => { w.projects = [project("alpha", "Example")]; });
    await t.open({ agent: "codex", path: "/workspace/example", project: "alpha", task });
    expect(t.world.creating()).toEqual([]);
    t.store.closeNewSession(); expect(t.world.creating()).toEqual([]);
    await t.open({ agent: "codex", path: "/workspace/example", project: "alpha", task });
    await t.start(); await t.start();
    expect(t.world.count("creationSubmit")).toBe(0);
    expect(t.world.of("taskExecutorSubmit")).toHaveLength(1);
    expect(t.world.of("taskExecutorSubmit")[0].args[0]).toMatchObject({ ...task, plan: { cmd: "codex" } });
    expect(t.world.admitted).toEqual(["creation-1"]);
  });
  it("retains an unknown task launch without submitting it again", async () => {
    const t = setup((w) => { w.admission = new Error("Task launch reply lost"); w.projects = [project("alpha", "Example")]; });
    await t.open({ agent: "codex", path: "/workspace/example", project: "alpha", task });
    await t.start(); await t.start();
    expect(t.world.count("taskExecutorSubmit")).toBe(1);
    expect(t.world.count("creationSubmit")).toBe(0);
    expect(t.state()).toMatchObject({ submitted: true, error: "Task launch reply lost" });
  });
});
