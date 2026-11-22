import { describe, expect, it } from "vitest";
import type { DraftMember, NewTeamView, TeamCall, TeamOutcome } from "../ui/teams/types";
import {
  createNewTeam,
  memberRowErrors,
  mergeDraft,
  settingsChanges,
  type ComputerChoice,
  type NewTeamFields,
  type NewTeamPort,
  portOn,
} from "./teamsNew";

describe("a form for another computer", () => {
  it("saves the draft and creates the team on that computer, not only starts it there", async () => {
    const sent: TeamCall[] = [];
    const done: TeamOutcome = { outcome: "done", receipt: { team: "t1", revision: 1 }, request_id: "" };
    const store = createNewTeam(portOn(() => "peer", async (call) => { sent.push(call); return done; }));
    store.openNew();
    store.edit("goal", "Ship it");
    await store.startPlanning({ folder: "/work/repo", lead: { role: "lead", agent: "claude" } as DraftMember });
    store.applyView(draftView({ team: "t1" }));
    store.edit("goal", "Ship it now");
    await store.createTeam();
    expect(sent.map((call) => [call.action.kind, call.computer])).toEqual([
      ["draft_start", "peer"],
      ["draft_update", "peer"],
      ["draft_create", "peer"],
    ]);
  });
});

const TEAM = "33fbafeb-8291-4eda-a00c-6e1448ec6f20";

class Port implements NewTeamPort {
  calls: TeamCall[] = [];
  reply: TeamOutcome = { outcome: "done", receipt: { team: TEAM }, request_id: "11111111-1111-4111-8111-111111111111" };
  async action(call: TeamCall): Promise<TeamOutcome> {
    this.calls.push(call);
    return this.reply;
  }
}

function member(overrides: Partial<DraftMember> = {}): DraftMember {
  return {
    id: "lead",
    role: "Lead",
    agent: "dsh",
    model: "deepseek-v4",
    effort: "high",
    account: "work",
    permissions: "bypass",
    computer: "studio",
    folder: "",
    prompt: "",
    lead: true,
    session: null,
    ...overrides,
  };
}

function draftView(overrides: Partial<NewTeamView> = {}): NewTeamView {
  return {
    team: TEAM,
    name: "Orbit",
    goal: "Ship it",
    addendum: "",
    folder: "C:\\plain-folder",
    settings: { branch_per_task: false, land: false },
    members: [member()],
    tasks: [],
    draft: { revision: 3, unseen: [], owner_edited: [] },
    validation: { errors: [], warnings: [] },
    updated: 0,
    ...overrides,
  };
}

const COMPUTERS: ComputerChoice[] = [
  { alias: "studio", label: "Studio", online: true },
  { alias: "laptop", label: "Laptop", online: false },
];

describe("New team creation rules", () => {
  it("creates nothing when the form is opened", () => {
    const port = new Port();
    const store = createNewTeam(port);
    store.openNew();
    expect(store.open()).toBe(true);
    expect(port.calls).toHaveLength(0);
  });

  it("Start planning sends only the draft start, in a non-Git folder", async () => {
    const port = new Port();
    const store = createNewTeam(port);
    store.openNew();
    const lead = member();
    const result = await store.startPlanning({ folder: "C:\\plain-folder", name: "Orbit", lead });
    expect(result.ok).toBe(true);
    expect(port.calls).toHaveLength(1);
    const call = port.calls[0];
    expect(call.action.kind).toBe("draft_start");
    if (call.action.kind !== "draft_start") throw new Error("kind");
    expect(call.action.folder).toBe("C:\\plain-folder");
    expect(call.action.lead).toBe(lead);
    expect(call.team).toBe("");
    expect(store.draftId()).toBe(TEAM);
    // No create, no member session and no worktree request is sent.
    expect(port.calls.some((item) => item.action.kind === "draft_create")).toBe(false);
    expect(port.calls.some((item) => JSON.stringify(item).includes("worktree"))).toBe(false);
  });

  it("refuses an empty folder before any call", async () => {
    const port = new Port();
    const store = createNewTeam(port);
    const result = await store.startPlanning({ folder: "  ", lead: member() });
    expect(result.ok).toBe(false);
    expect(port.calls).toHaveLength(0);
  });

  it("Create team sends only the draft create once the draft is read", async () => {
    const port = new Port();
    const store = createNewTeam(port);
    store.applyView(draftView());
    const result = await store.createTeam();
    expect(result.ok).toBe(true);
    expect(port.calls.map((call) => call.action.kind)).toEqual(["draft_create"]);
  });

  it("flushes the owner's dirty fields before Create team", async () => {
    const port = new Port();
    const store = createNewTeam(port);
    store.applyView(draftView());
    store.edit("goal", "My unsaved goal");
    await store.createTeam();
    expect(port.calls.map((call) => call.action.kind)).toEqual(["draft_update", "draft_create"]);
    const update = port.calls[0];
    if (update.action.kind !== "draft_update") throw new Error("kind");
    expect(update.action.revision).toBe(3);
    expect(update.action.set).toEqual({ goal: "My unsaved goal" });
  });

  it("shows member row errors live and allows a non-Git folder", () => {
    expect(memberRowErrors(member({ role: "" }), COMPUTERS)).toEqual([{ code: "role" }]);
    expect(memberRowErrors(member({ computer: "laptop" }), COMPUTERS)).toEqual([
      { code: "computer-offline", alias: "laptop" },
    ]);
    expect(memberRowErrors(member({ computer: "studio" }), COMPUTERS)).toEqual([]);
  });
});

describe("draft polling", () => {
  it("keeps the owner's unsaved fields and takes every other lead edit in one poll", () => {
    const store = createNewTeam(new Port());
    store.applyView(draftView());
    store.edit("goal", "My unsaved goal");
    store.edit("settings", { branch_per_task: false, land: true });
    const polled = draftView({
      name: "Orbit by the lead",
      goal: "The lead's goal",
      settings: { branch_per_task: true, land: false },
      members: [member({ role: "Lead" }), member({ id: "backend", role: "Backend", lead: false })],
      draft: { revision: 4, unseen: ["name", "goal", "settings", "members"], owner_edited: ["goal"] },
    });
    store.applyView(polled);
    expect(store.fields().name).toBe("Orbit by the lead");
    expect(store.fields().goal).toBe("My unsaved goal");
    expect(store.fields().settings).toEqual({ branch_per_task: false, land: true });
    expect(store.fields().members).toHaveLength(2);
    expect(store.unseen()).toEqual(["name", "goal", "settings", "members"]);
  });

  it("Task #308: shows the project's working mode and keeps the owner's choice", () => {
    const store = createNewTeam(new Port());
    store.applyView(draftView({ working_mode: { accepted: "auto", preset: "branch-pipeline",
      proposal: { preset: "branch-pipeline", title: "Branch pipeline", reasons: ["A code repository with tests"] } } }));
    expect(store.fields().working_mode).toBe("auto");
    store.edit("working_mode", "shared-folder");
    store.applyView(draftView({ working_mode: { accepted: "auto", preset: "branch-pipeline",
      proposal: { preset: "branch-pipeline", title: "Branch pipeline", reasons: ["A code repository with tests"] } } }));
    expect(store.fields().working_mode).toBe("shared-folder");
  });

  it("merges a polled view without touching dirty fields", () => {
    const local: NewTeamFields = {
      name: "Local",
      goal: "Local goal",
      addendum: "",
      folder: "",
      settings: { land: false },
      members: [],
      tasks: [],
      working_mode: "auto",
      plan_mode: "standard",
    };
    const merged = mergeDraft(local, new Set(["name", "goal"]), draftView());
    expect(merged.name).toBe("Local");
    expect(merged.goal).toBe("Local goal");
    expect(merged.members).toHaveLength(1);
    expect(merged.folder).toBe("C:\\plain-folder");
  });
});

describe("plan mode (task #8, team ede724c4)", () => {
  it("follows the draft's plan mode until the owner changes it, then keeps the owner's", () => {
    const store = createNewTeam(new Port());
    expect(store.fields().plan_mode).toBe("standard");
    store.applyView(draftView({ plan_mode: "free", plan_mode_offer: "", plan_mode_reason: "" }));
    expect(store.fields().plan_mode).toBe("free");
    store.edit("plan_mode", "standard");
    store.applyView(draftView({ plan_mode: "free", plan_mode_offer: "free", plan_mode_reason: "a stream of requests",
      draft: { revision: 4, unseen: ["plan_mode", "plan_mode_reason"], owner_edited: [] } }));
    expect(store.fields().plan_mode).toBe("standard");
    expect(store.unseen()).toEqual(["plan_mode", "plan_mode_reason"]);
    // An older hgs leaves the field out: standard.
    expect(mergeDraft(store.fields(), new Set(), draftView()).plan_mode).toBe("standard");
  });

  it("saves a plan mode chosen before Start planning with the draft's first update", async () => {
    const port = new Port();
    const store = createNewTeam(port);
    store.openNew();
    store.edit("plan_mode", "free");
    await store.startPlanning({ folder: "C:\\plain-folder", lead: member() });
    expect(port.calls.map((call) => call.action.kind)).toEqual(["draft_start"]);
    await store.flush();
    const update = port.calls[1];
    if (update?.action.kind !== "draft_update") throw new Error("kind");
    expect((update.action.set as Record<string, unknown>).plan_mode).toBe("free");
  });

  it("marks the plan mode and its reason seen with one call", async () => {
    const port = new Port();
    const store = createNewTeam(port);
    store.applyView(draftView({ plan_mode_offer: "free", plan_mode_reason: "small requests",
      draft: { revision: 3, unseen: ["goal", "plan_mode", "plan_mode_reason"], owner_edited: [] } }));
    await store.markSeen(["plan_mode", "plan_mode_reason"]);
    expect(store.unseen()).toEqual(["goal"]);
    expect(port.calls).toHaveLength(1);
    const seen = port.calls[0].action;
    if (seen.kind !== "draft_seen") throw new Error("kind");
    expect(seen.fields).toEqual(["plan_mode", "plan_mode_reason"]);
  });
});

describe("team settings", () => {
  it("sends only the changed settings to hgs team settings", async () => {
    const port = new Port();
    const store = createNewTeam(port);
    store.applyView(draftView({ settings: { branch_per_task: false, land: false } }));
    await store.saveSettings({ branch_per_task: false, land: true, restart_crashed: true });
    expect(port.calls).toHaveLength(1);
    const call = port.calls[0];
    expect(call.team).toBe(TEAM);
    expect(call.action.kind).toBe("settings");
    if (call.action.kind !== "settings") throw new Error("kind");
    expect(call.action.settings).toEqual({ land: true, restart_crashed: true });
  });

  it("round-trips the saved settings back into the open form", () => {
    const store = createNewTeam(new Port());
    store.applyView(draftView({ settings: { land: false } }));
    const after = draftView({ settings: { land: true }, draft: { revision: 4, unseen: ["settings"], owner_edited: [] } });
    store.applyView(after);
    expect(store.fields().settings).toEqual({ land: true });
    expect(store.unseen()).toEqual(["settings"]);
  });

  it("computes changes without dropping an explicitly cleared value", () => {
    expect(settingsChanges({ branch: "main", land: false }, { branch: "next", land: false })).toEqual({ branch: "next" });
    expect(settingsChanges({ intake: "code" }, { intake: null as unknown as string })).toEqual({ intake: null });
  });
});

describe("reopening a draft (task 200, Qt TeamNewTab::openDraft)", () => {
  it("shows the draft as its view brings it, sends nothing and saves against its revision", async () => {
    const port = new Port();
    const store = createNewTeam(port);
    store.openNew();
    store.edit("goal", "unsaved, from another form");
    store.openDraft(TEAM);
    expect(store.draftId()).toBe(TEAM);
    expect(store.fields().goal).toBe("");
    expect(store.dirty().size).toBe(0);
    expect(port.calls).toEqual([]);
    store.applyView(draftView());
    expect([store.fields().name, store.fields().goal, store.fields().folder, store.revision()]).toEqual(["Orbit", "Ship it", draftView().folder, 3]);
    store.edit("goal", "Ship it soon");
    await store.flush();
    expect(port.calls).toHaveLength(1);
    expect(port.calls[0]).toMatchObject({ team: TEAM, action: { kind: "draft_update", revision: 3 } });
  });

  it("keeps the open draft when it is asked again and ignores an empty id", () => {
    const store = createNewTeam(new Port());
    store.openDraft(TEAM);
    store.applyView(draftView());
    store.edit("name", "Orbit two");
    store.openDraft(TEAM);
    store.openDraft("");
    expect([store.draftId(), store.fields().name, store.dirty().has("name")]).toEqual([TEAM, "Orbit two", true]);
  });
});
