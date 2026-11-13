import { describe, expect, it } from "vitest";
import { toSession } from "../../store/catalog";
import {
  canStartStopped, headerClearSlot, sessionIdentity, copyState, detailsFromInspection, freshCopy, freshEntry, freshStillValid, identityOk, notificationsState, notificationsStillAllowed, dialogReducer, menuItemActivates, menuState, renameParts, renameProblem, snapshotOf, tagProblem, terminal, terminateCopy, forgetCopy, forgetsBindingOnly, forgetOpening, forgetArchivedCopy, archiveNameOccupied, liveNameIndex, nameKey,
} from "./actions-ui";
import fixture from "../../../core/tests/fixtures/action-rows.json";
import type { Details, ViewRow } from "../../types";

const row = (extra: Partial<ViewRow> = {}, computer = "") => toSession(computer, "run:a", {
  name: "claude/proj/a", agent: "claude", state: "running", resumable: true, activity: "idle", process_state: "running", run_id: "r1",
  conversation_id: "c1", machine: "box", ...extra,
});
const details = (extra: Partial<Details> = {}): Details => ({
  run_id: "r1", conversation_id: "c1", auth_required: false, expected_id: "", resume_message_can_send: true, allowed_actions: null, action_reasons: {},
  interrupt_supported: false, turn_started: 0, phase: "", ...extra,
});

describe("menuState change", () => {
  it("offers Pause before the first turn only from an explicit native capability", () => {
    const fresh = row({ conversation_id: null, resumable: false, activity: "unknown", phase: "unknown", allowed_actions: ["pause", "terminate"] });
    expect(menuState(fresh, true, false).change).toMatchObject({ label: "Pause session", enabled: true });
    expect(menuState(fresh, true, false, details({ conversation_id: "", allowed_actions: ["terminate"] })).change.enabled).toBe(false);
    expect(menuState(row({ conversation_id: null, resumable: false, activity: "unknown" }), true, false).change.enabled).toBe(false);
    expect(menuState(fresh, false, false).change.enabled).toBe(false);
    expect(menuState(fresh, true, true).change.enabled).toBe(false);
    expect(menuState({ ...fresh, processState: "exited" }, true, false).change.enabled).toBe(false);
  });
  it("pauses an idle running claude", () => {
    expect(menuState(row(), true, false).change).toMatchObject({ label: "Pause session", enabled: true });
  });
  it("does not pause a working one", () => expect(menuState(row({ activity: "busy" }), true, false).change.enabled).toBe(false));
  it("does not pause an exited process", () => expect(menuState(row({ process_state: "exited" }), true, false).change.enabled).toBe(false));
  it("resumes a paused resumable session", () => {
    expect(menuState(row({ state: "paused" }), true, false).change).toMatchObject({ label: "Resume session", enabled: true });
    expect(menuState(row({ state: "paused", resumable: false }), true, false).change.enabled).toBe(false);
  });
  it("is Updating and disabled while updating or offline", () => {
    expect(menuState(row(), true, true).change).toMatchObject({ label: "Updating…", enabled: false });
    expect(menuState(row(), false, false).change.enabled).toBe(false);
  });
  it("restores an archived row (Qt canChange 5302-5304, the header 3897-3898)", () => {
    const archived = row({ state: "archived", archive_id: "a1" });
    expect(menuState(archived, true, false).change).toEqual({ label: "Restore session", enabled: true, reason: undefined });
    expect(menuState(archived, true, false, undefined, { context: true }).change.enabled).toBe(true);
    // Not without its archive id, not resumable, offline, updating or while a batch runs.
    expect(menuState(row({ state: "archived" }), true, false).change.enabled).toBe(false);
    expect(menuState(row({ state: "archived" }), true, false, undefined, { context: true }).change.enabled).toBe(false);
    expect(menuState(row({ state: "archived", archive_id: "a1", resumable: false }), true, false).change.enabled).toBe(false);
    expect(menuState(archived, false, false).change.enabled).toBe(false);
    expect(menuState(archived, true, true).change).toMatchObject({ label: "Updating…", enabled: false });
    expect(menuState(archived, true, false, undefined, { batchRunning: true, context: true }).change.enabled).toBe(false);
  });
  it("does not restore while a live row of that computer has the name (archiveNameOccupied)", () => {
    const archived = row({ state: "archived", archive_id: "a1" });
    expect(menuState(archived, true, false, undefined, { occupied: true }).change.enabled).toBe(false);
    expect(menuState(archived, true, false, undefined, { occupied: true, context: true }).change.enabled).toBe(false);
    // A live row's change entry never reads it.
    expect(menuState(row({ state: "paused" }), true, false, undefined, { occupied: true }).change.enabled).toBe(true);
  });
  it("follows the computer's restore decision, as batch Restore does (zerus-wyrqg)", () => {
    // An archive that ended before its first message saved no conversation.
    const empty = "This session ended before its first message, so it saved no conversation to restore";
    const refused = row({ state: "archived", archive_id: "a1", allowed_actions: ["rename", "forget"], action_reasons: { restore: empty } });
    expect(menuState(refused, true, false, undefined, { context: true }).change).toEqual({ label: "Restore session", enabled: false, reason: empty });
    expect(menuState(refused, true, false).change).toEqual({ label: "Restore session", enabled: false, reason: empty });
    // Details win over the row.
    expect(menuState(refused, true, false, details({ allowed_actions: ["restore"] })).change.enabled).toBe(true);
    const allowed = row({ state: "archived", archive_id: "a1", allowed_actions: ["restore", "rename", "forget"] });
    expect(menuState(allowed, true, false).change).toEqual({ label: "Restore session", enabled: true, reason: undefined });
    // A computer that reports no actions keeps Qt's rule (canChange 5302-5304).
    expect(menuState(row({ state: "archived", archive_id: "a1" }), true, false).change.enabled).toBe(true);
    // The other conditions still hold.
    expect(menuState(row({ state: "archived", allowed_actions: ["restore"] }), true, false, undefined, { context: true }).change.enabled).toBe(false);
    expect(menuState(allowed, true, false, undefined, { context: true, occupied: true }).change.enabled).toBe(false);
  });
  it("lets auth_required stand in for idle with details", () => {
    expect(menuState(row({ activity: "busy" }), true, false, details({ auth_required: true })).change.enabled).toBe(true);
  });
  it("needs the resume message when an expected id is set", () => {
    expect(menuState(row(), true, false, details({ expected_id: "x", resume_message_can_send: false })).change.enabled).toBe(false);
    expect(menuState(row(), true, false, details({ expected_id: "x", resume_message_can_send: true })).change.enabled).toBe(true);
  });
  it("follows reported allowed actions, details first", () => {
    expect(menuState(row({ allowed_actions: ["rename"] }), true, false).change.enabled).toBe(false);
    expect(menuState(row({ allowed_actions: ["rename"] }), true, false, details({ allowed_actions: ["pause"] })).change.enabled).toBe(true);
    expect(menuState(row({ allowed_actions: [] }), true, false).rename).toBe(false);
  });
  it("lets an opencode session only rename and forget", () => {
    const m = menuState(row({ agent: "opencode" }), true, false);
    expect(m.change.enabled).toBe(false);
    expect(m.rename).toBe(true);
    expect(m.end.label).toBe("Forget saved session…");
  });
});

describe("menuState rename, archive, end, read", () => {
  it("renames with an agent and a project", () => {
    expect(menuState(row(), true, false).rename).toBe(true);
    expect(menuState(row({ name: "claude" }), true, false).rename).toBe(false);
    expect(menuState(row({ name: "/a" }), true, false).rename).toBe(false);
    expect(menuState(row(), false, false).rename).toBe(false);
  });
  it("renames an archived row only with its archive id", () => {
    expect(menuState(row({ state: "archived" }), true, false).rename).toBe(false);
    expect(menuState(row({ state: "archived", archive_id: "a1" }), true, false).rename).toBe(true);
  });
  it("offers archive for paused or stopped, not DeepSeek", () => {
    expect(menuState(row({ state: "paused" }), true, false).archive).toMatchObject({ visible: true, enabled: true });
    expect(menuState(row({ state: "stopped" }), true, false).archive.visible).toBe(true);
    expect(menuState(row(), true, false).archive.visible).toBe(false);
    // The core names DeepSeek's `dsh` sessions `deepseek`.
    expect(menuState(row({ state: "paused", agent: "deepseek" }), true, false).archive.visible).toBe(false);
  });
  it("needs the machine online for Move to archive in the context menu only", () => {
    expect(menuState(row({ state: "paused" }), false, false).archive.enabled).toBe(true);
    expect(menuState(row({ state: "paused" }), false, false, undefined, { context: true }).archive.enabled).toBe(false);
    expect(menuState(row({ state: "paused" }), true, false, undefined, { context: true }).archive.enabled).toBe(true);
  });
  it("passes the archive reason, details first", () => {
    expect(menuState(row({ state: "paused", allowed_actions: [], action_reasons: { archive: "No." } }), true, false).archive).toMatchObject({ enabled: false, reason: "No." });
    const m = menuState(row({ state: "paused", allowed_actions: [], action_reasons: { archive: "row" } }), true, false, details({ allowed_actions: [], action_reasons: { archive: "detail" } }));
    expect(m.archive.reason).toBe("detail");
  });
  it("terminates a running session", () => {
    expect(menuState(row(), true, false).end).toMatchObject({ label: "Terminate session…", enabled: true });
    expect(menuState(row(), false, false).end.enabled).toBe(false);
    expect(menuState(row({ allowed_actions: ["pause"], action_reasons: { terminate: "Not now" } }), true, false).end).toMatchObject({ enabled: false, reason: "Not now" });
  });
  it("enables the saved-session forget by allowed(forget) with Qt's reason otherwise", () => {
    expect(menuState(row({ agent: "opencode" }), true, false).end).toMatchObject({ label: "Forget saved session…", enabled: true });
    expect(menuState(row({ state: "paused" }), true, false).end).toMatchObject({ label: "Forget saved session…", enabled: true });
    expect(menuState(row({ state: "paused" }), false, false).end.enabled).toBe(false);
    expect(menuState(row({ state: "paused" }), true, true).end.enabled).toBe(false);
    const no = menuState(row({ state: "paused", allowed_actions: ["rename"] }), true, false).end;
    expect(no).toMatchObject({ enabled: false, reason: "This native adapter does not support this action in the current session state." });
    expect(menuState(row({ state: "paused", allowed_actions: ["rename"] }), true, false, details({ action_reasons: { forget: "Busy" } })).end.reason).toBe("Busy");
    expect(menuState(row({ state: "paused", allowed_actions: ["rename"] }), true, false, details({ allowed_actions: ["forget"] })).end.enabled).toBe(true);
  });
  it("forgets an archived row with allowed(forget) and its archive id (Qt 5378-5380)", () => {
    const archived = row({ state: "archived", archive_id: "a1" });
    expect(menuState(archived, true, false).end).toEqual({ label: "Forget archived session…", enabled: true, reason: undefined });
    expect(menuState(archived, true, false, undefined, { context: true }).end.enabled).toBe(true);
    expect(menuState(row({ state: "archived" }), true, false).end).toMatchObject({ label: "Forget archived session…", enabled: false });
    expect(menuState(archived, false, false).end.enabled).toBe(false);
    expect(menuState(archived, true, true).end.enabled).toBe(false);
    expect(menuState(archived, true, false, undefined, { batchRunning: true }).end.enabled).toBe(false);
    const refused = menuState(row({ state: "archived", archive_id: "a1", allowed_actions: ["restore"], action_reasons: { forget: "Kept." } }), true, false).end;
    expect(refused).toMatchObject({ enabled: false, reason: "Kept." });
  });
});

describe("menuState stop", () => {
  const working = { phase: "tool", activity: "busy" } as const;
  it("is offered while the turn works and enabled when the host can interrupt it", () => {
    expect(menuState(row(working), true, false, details({ interrupt_supported: true })).stop).toEqual({ visible: true, enabled: true, reason: undefined });
  });
  it("stays off without details, since hosts never list interrupt as an allowed action", () => {
    expect(menuState(row({ ...working, allowed_actions: ["rename", "terminate", "forget"] }), true, false).stop)
      .toEqual({ visible: true, enabled: false, reason: "Interrupt is unavailable. Open Terminal to stop this turn." });
  });
  it("gives Qt's reasons when it is off", () => {
    expect(menuState(row(working), false, false, details({ interrupt_supported: true })).stop).toMatchObject({ enabled: false, reason: "Machine is offline" });
    expect(menuState(row(working), true, true, details({ interrupt_supported: true }), { interrupting: true }).stop)
      .toMatchObject({ enabled: false, reason: "Interrupt is being sent…" });
  });
  it("waits only for its own interrupt, not for another write (Qt's m_interruptRequest)", () => {
    expect(menuState(row(working), true, true, details({ interrupt_supported: true })).stop.enabled).toBe(true);
  });
  it("follows the inspected phase and a running state", () => {
    expect(menuState(row(), true, false, details({ interrupt_supported: true, phase: "compacting" })).stop.enabled).toBe(true);
    expect(menuState(row(working), true, false, details({ interrupt_supported: true, phase: "idle" })).stop.visible).toBe(false);
    expect(menuState(row({ ...working, state: "paused" }), true, false, details({ interrupt_supported: true })).stop.visible).toBe(false);
  });
});

describe("menuState while a selection batch runs (Qt's m_pending)", () => {
  const batch = { batchRunning: true };
  it("turns every write entry off", () => {
    const paused = row({ state: "paused" });
    expect(menuState(row(), true, false, undefined, batch).change.enabled).toBe(false);
    expect(menuState(paused, true, false, undefined, batch).change.enabled).toBe(false);
    expect(menuState(row(), true, false, undefined, batch).rename).toBe(false);
    expect(menuState(paused, true, false, undefined, batch).archive.enabled).toBe(false);
    expect(menuState(paused, true, false, undefined, { ...batch, context: true }).archive.enabled).toBe(false);
    expect(menuState(row(), true, false, undefined, batch).end.enabled).toBe(false);
    expect(menuState(paused, true, false, undefined, batch).end.enabled).toBe(false);
  });
  it("reads Updating in the header and keeps the context menu's words", () => {
    expect(menuState(row(), true, false, undefined, batch).change.label).toBe("Updating…");
    expect(menuState(row(), true, false, undefined, { ...batch, context: true }).change.label).toBe("Pause session");
  });
  it("leaves Stop alone: an interrupt is not a write", () => {
    const working = row({ phase: "tool", activity: "busy" });
    expect(menuState(working, true, false, details({ interrupt_supported: true }), batch).stop.enabled).toBe(true);
  });
  it("changes nothing once the batch ends", () => {
    expect(menuState(row(), true, false, undefined, { batchRunning: false })).toEqual(menuState(row(), true, false));
  });
});

describe("terminateCopy", () => {
  it("stops a tracked claude into Archive", () => {
    expect(terminateCopy(row({ tracked: true }))).toEqual({
      title: "Terminate session?",
      text: "Stop claude/proj/a and move its recorded conversation to Archive? Running processes will stop; the conversation history is kept.",
      detail: "Computer: box", confirm: "Stop and archive", action: "terminate",
    });
  });
  it("kills any other live session", () => {
    const c = terminateCopy(row({ tracked: false }));
    expect(c.text).toBe("Terminate claude/proj/a? Its running processes will stop. No HGS archive is available for this session; any native conversation history is kept.");
    expect(c.confirm).toBe("Terminate session");
    expect(c.action).toBe("kill");
    expect(terminateCopy(row({ tracked: true, agent: "opencode" })).action).toBe("kill");
    expect(terminateCopy(row({ tracked: true, agent: "deepseek" })).action).toBe("kill");
  });
});

describe("rename", () => {
  it("edits only the tag after the agent and project", () => {
    expect(renameParts("claude/proj/a")).toEqual({ prefix: "claude/proj/", tag: "a" });
    expect(renameParts("claude/proj")).toEqual({ prefix: "claude/proj/", tag: "" });
  });
  it("follows SessionTag::problem", () => {
    expect(tagProblem("release planning")).toBeUndefined();
    expect(tagProblem("")).toBe("Enter a session name.");
    for (const t of [" a", "a ", "\ta"]) expect(tagProblem(t)).toBe("Remove spaces at the beginning or end.");
    for (const t of ["a/b", "a:b", "v1.2", "a\u0007b"]) expect(tagProblem(t)).toBe("Names cannot contain /, : or . or control characters.");
  });
  it("refuses a live name already used on the same computer", () => {
    const s = row();
    const other = (name: string, key: string, extra: Partial<ViewRow> = {}, computer = "") => ({ ...row({ name, ...extra }, computer), key });
    const others = [other("claude/proj/b", "\nrun:b"), other("claude/proj/c", "laptop\nrun:c", {}, "laptop"), other("claude/proj/d", "\nrun:d", { state: "archived" })];
    expect(renameProblem("b", s, others)).toBe("This name is already in use on box.");
    expect(renameProblem("c", s, others)).toBeUndefined();
    expect(renameProblem("d", s, others)).toBeUndefined();
    expect(renameProblem("a", s, [s])).toBeUndefined();
    expect(renameProblem("b", row({ state: "archived", archive_id: "a1" }), others)).toBeUndefined();
    expect(renameProblem("b.c", s, others)).toBe("Names cannot contain /, : or . or control characters.");
  });
});

describe("details", () => {
  it("reads the inspection fields of an inspect reply or a state value", () => {
    expect(detailsFromInspection({ run_id: "r", conversation_id: "c", phase: "tool", turn_started: 1.5, interrupt_supported: true, allowed_actions: ["pause", 3], action_reasons: { resume: "busy", x: 1 } }))
      .toEqual({ run_id: "r", conversation_id: "c", auth_required: false, expected_id: "", resume_message_can_send: false, fork_supported: false, fork_reason: "", allowed_actions: ["pause"],
        action_reasons: { resume: "busy" }, interrupt_supported: true, turn_started: 1.5, phase: "tool", cwd: null, terminal_supported: null, terminal_attach_supported: null });
    expect(detailsFromInspection(null)).toBeUndefined();
    expect(detailsFromInspection([1])).toBeUndefined();
  });
});

describe("dialog helpers", () => {
  it("opens one dialog at a time", () => {
    const a = snapshotOf(row());
    let state = dialogReducer(undefined, { type: "open", kind: "rename", row: a });
    expect(state?.kind).toBe("rename");
    state = dialogReducer(state, { type: "open", kind: "terminate", row: a });
    expect(state).toEqual({ kind: "terminate", row: a });
    expect(dialogReducer(state, { type: "close" })).toBeUndefined();
  });
  it("opens the selection confirmation as one more dialog, replacing what is open", () => {
    const a = snapshotOf(row());
    const copy = { title: "Forget", text: "Forget 1 selected sessions?", details: ["box / claude/proj/a"], confirm: "Forget 1 sessions" };
    const answer = () => {};
    let state = dialogReducer(undefined, { type: "open", kind: "rename", row: a });
    state = dialogReducer(state, { type: "open", kind: "batch", copy, answer });
    expect(state).toEqual({ kind: "batch", copy, answer });
    expect(dialogReducer(state, { type: "open", kind: "terminate", row: a })).toEqual({ kind: "terminate", row: a });
    expect(dialogReducer(state, { type: "close" })).toBeUndefined();
  });
  it("ignores disabled menu items", () => {
    expect(menuItemActivates({ disabled: true })).toBe(false);
    expect(menuItemActivates({})).toBe(true);
  });
  it("snapshots plain values", () => {
    const s = row({ reply_id: "r1" });
    const snap = snapshotOf(s);
    expect(snap).toMatchObject({ key: s.key, replyId: "r1", runId: "r1", cmd: "claude" });
    expect(Object.keys(snap)).not.toContain("title");
  });
});

// The rows the core projects from real hgs listings (core/tests/fixtures,
// checked against view.rs), so these menus see the values hgs sends.
describe("menus from hgs rows as the core projects them", () => {
  const cases = Object.fromEntries(fixture.rows.map((c) => [c.case, c]));
  const projected = (name: string) => {
    const c = cases[name];
    return { s: toSession(c.computer, c.key, c.view as ViewRow), state: "state" in c ? c.state : undefined };
  };

  it("stops a working Claude turn through its inspected details, never through allowed_actions", () => {
    const { s, state } = projected("a working tracked Claude turn");
    expect(s.allowed?.split(" ")).not.toContain("interrupt");
    const d = detailsFromInspection(state)!;
    expect(d).toMatchObject({ run_id: s.runId, conversation_id: s.conversationId, interrupt_supported: true, turn_started: 1793000000.5 });
    const m = menuState(s, true, false, d);
    expect(m.stop).toEqual({ visible: true, enabled: true, reason: undefined });
    expect(menuState(s, true, false).stop.enabled).toBe(false);
    expect(m.change).toMatchObject({ label: "Pause session", enabled: false });
    expect(m.end).toMatchObject({ label: "Terminate session…", enabled: true });
    expect(m.rename).toBe(true);
    expect(terminateCopy(s).action).toBe("terminate");
  });

  it("kills a live session hgs does not track", () => {
    const { s } = projected("a live session hgs does not track");
    expect(s.state).toBe("running");
    expect(s.runId).toBeUndefined();
    const m = menuState(s, true, false, undefined, { context: true });
    expect(m.end).toMatchObject({ label: "Terminate session…", enabled: true });
    expect(m.rename).toBe(true);
    expect(m.stop.visible).toBe(false);
    expect(terminateCopy(s)).toMatchObject({ action: "kill", confirm: "Terminate session" });
  });

  it("never offers archive for DeepSeek and resumes it as reported", () => {
    const { s } = projected("a paused DeepSeek session");
    expect(s.cmd).toBe("deepseek");
    const m = menuState(s, true, false, undefined, { context: true });
    expect(m.archive.visible).toBe(false);
    expect(m.change).toMatchObject({ label: "Resume session", enabled: true });
    expect(m.end).toMatchObject({ label: "Forget saved session…" });
  });

  it("forgets only the binding of an OpenCode session", () => {
    const { s } = projected("an idle OpenCode session");
    expect(s.cmd).toBe("opencode");
    const m = menuState(s, true, false);
    expect(m.change.enabled).toBe(false);
    expect(m.rename).toBe(true);
    expect(m.end).toMatchObject({ label: "Forget saved session…", enabled: true });
  });
});

describe("terminal", () => {
  it("is never offered for deepseek", () => {
    expect(terminal(row({ agent: "deepseek", terminal_supported: true, terminal_reported: true }), details({ terminal_supported: true }))).toBe(false);
    expect(terminal(row({ agent: "dsh", terminal_supported: true }))).toBe(false);
  });
  it("follows the row for other agents and lets the details win", () => {
    expect(terminal(row({ terminal_supported: true }))).toBe(true);
    expect(terminal(row({ terminal_supported: false }))).toBe(false);
    expect(terminal(row({ terminal_supported: false }), details({ terminal_supported: true }))).toBe(true);
    expect(terminal(row({ terminal_supported: true }), details({ terminal_supported: false }))).toBe(false);
    expect(terminal(row({ terminal_supported: true }), details({ terminal_supported: null }))).toBe(true);
  });
  it("reads the attach support first for opencode, details before the row", () => {
    const oc = (extra: Partial<ViewRow>) => row({ agent: "opencode", ...extra });
    expect(terminal(oc({ terminal_attach_supported: true, terminal_attach_reported: true }))).toBe(true);
    expect(terminal(oc({ terminal_attach_supported: true, terminal_attach_reported: true }), details({ terminal_attach_supported: false }))).toBe(false);
    expect(terminal(oc({ terminal_attach_supported: false, terminal_attach_reported: true, terminal_supported: true, terminal_reported: true }))).toBe(false);
    expect(terminal(oc({}), details({ terminal_attach_supported: true }))).toBe(true);
  });
  it("gives an opencode row without a reported value no terminal", () => {
    expect(terminal(row({ agent: "opencode", terminal_supported: true }))).toBe(false);
    expect(terminal(row({ agent: "opencode", terminal_supported: true, terminal_reported: true }))).toBe(true);
    expect(terminal(row({ agent: "opencode" }), details({ terminal_supported: true }))).toBe(true);
  });
});

describe("header session identity", () => {
  it("copies the conversation ID, preferring inspected identity", () => {
    expect(sessionIdentity(row())).toEqual({ value: "c1", title: "Session ID", caption: "Copy session ID" });
    expect(sessionIdentity(row(), details({ conversation_id: "inspected" })).value).toBe("inspected");
  });
  it("falls back to run ID with its own caption when no conversation is known", () => {
    expect(sessionIdentity(row(), details({ conversation_id: "", run_id: "inspected-run" }))).toEqual({ value: "inspected-run", title: "Run ID", caption: "Copy run ID" });
    expect(sessionIdentity(row({ conversation_id: undefined }))).toEqual({ value: "r1", title: "Run ID", caption: "Copy run ID" });
  });
  it("a child uses its own conversation ID or agent ID, never the parent's", () => {
    expect(sessionIdentity(row(), details(), { id: "agent-1", conversationId: "child-conversation" })).toEqual({ value: "child-conversation", title: "Session ID", caption: "Copy session ID" });
    expect(sessionIdentity(row(), details(), { id: "agent-1" })).toEqual({ value: "agent-1", title: "Agent ID", caption: "Copy agent ID" });
  });
  it("has no copy value when neither conversation nor run is confirmed", () => {
    expect(sessionIdentity(row({ conversation_id: undefined, run_id: undefined }))).toEqual({ value: "", title: "Session ID", caption: "Copy session ID" });
  });
});

describe("copyState", () => {
  it("offers connect for a terminal row and restore for an archived one", () => {
    expect(copyState(row({ terminal_supported: true }), true).command).toEqual({ label: "Copy connect command", enabled: true });
    expect(copyState(row({ terminal_supported: false }), true).command).toEqual({ label: "Copy connect command", enabled: false });
    expect(copyState(row({ state: "archived", archive_id: "a1", terminal_supported: false }), true).command).toEqual({ label: "Copy restore command", enabled: true });
  });
  it("needs an archive id for an archived row", () => {
    expect(copyState(row({ state: "archived", terminal_supported: true }), true).command.enabled).toBe(false);
  });
  it("copies the name always and the reference only online with a run id", () => {
    expect(copyState(row(), false).reference).toBe(false);
    expect(copyState(row({ run_id: undefined }), true).reference).toBe(false);
    expect(copyState(row(), true).reference).toBe(true);
  });
  it("copies the details folder first, else the row's, and is off when empty", () => {
    expect(copyState(row({ cwd: "/home/example/row" }), true).folder).toEqual({ enabled: true, path: "/home/example/row" });
    expect(copyState(row({ cwd: "/home/example/row" }), true, details({ cwd: "/home/example/detail" })).folder.path).toBe("/home/example/detail");
    expect(copyState(row({ cwd: "/home/example/row" }), true, details({ cwd: null })).folder.path).toBe("/home/example/row");
    expect(copyState(row({ cwd: "/home/example/row" }), true, details({ cwd: "" })).folder).toEqual({ enabled: false, path: "" });
    expect(copyState(row(), true).folder).toEqual({ enabled: false, path: "" });
  });
  it("keeps a present empty cwd empty and an absent one null", () => {
    expect(detailsFromInspection({ cwd: "" })?.cwd).toBe("");
    expect(detailsFromInspection({})?.cwd).toBeNull();
  });
  it("reads the inspection cwd and terminal flags", () => {
    expect(detailsFromInspection({ cwd: "/home/example/x", terminal_supported: false, terminal_attach_supported: true }))
      .toMatchObject({ cwd: "/home/example/x", terminal_supported: false, terminal_attach_supported: true });
  });
});

describe("notificationsState", () => {
  const free = { conversation: false, total: 0 };
  const shared = (extra: Partial<ViewRow> = {}) => row({ shared_notifications: true, notification_revision: 2, ...extra });
  it("is Mute or Unmute with Qt's hints when available", () => {
    expect(notificationsState(shared(), true, free)).toEqual({ label: "Mute", enabled: true, hint: "Mute notifications for this session on all devices" });
    expect(notificationsState(shared({ muted: true }), true, free)).toMatchObject({ label: "Unmute", enabled: true, hint: "Allow future notifications for this session on all devices" });
  });
  it("is absent for an archived row", () => expect(notificationsState(shared({ state: "archived" }), true, free)).toBeUndefined());
  it("gives the reasons in Qt's order", () => {
    const pending = { conversation: true, total: 1 };
    const bare = row({ run_id: "", conversation_id: "" });
    expect(notificationsState(bare, false, pending)?.hint).toBe("Connect to this session's host to change shared notifications.");
    expect(notificationsState(bare, true, pending)?.hint).toBe("Update hgs on this session's host to support shared notification preferences.");
    expect(notificationsState(shared({ run_id: "" }), true, pending)).toMatchObject({ enabled: false, hint: "Refresh this session's identity before changing shared notifications." });
    const conflict = { ...snapshotOf(shared()), notificationConflict: true };
    expect(notificationsState(conflict, true, pending)).toMatchObject({ enabled: false, hint: "Conflicting shared notification preferences. Refresh the session." });
    expect(notificationsState(shared(), true, pending)).toMatchObject({ enabled: false, hint: "A shared notification change is pending. Wait for its result." });
    expect(notificationsState(shared(), true, { conversation: false, total: 16 })).toMatchObject({ enabled: false, hint: "A shared notification change is pending. Wait for its result." });
    expect(notificationsState(shared(), true, { conversation: false, total: 15 })?.enabled).toBe(true);
  });
  it("checks identities as Qt does", () => {
    expect(identityOk("x".repeat(128), 128)).toBe(true);
    expect(identityOk("x".repeat(129), 128)).toBe(false);
    expect(identityOk("\u00e9".repeat(65), 128)).toBe(false);
    expect(identityOk("", 128)).toBe(false);
    expect(identityOk("a\tb", 128)).toBe(false);
    expect(identityOk("a\u007fb", 128)).toBe(false);
    expect(identityOk(undefined, 128)).toBe(false);
  });
  it("rechecks the run and the preferences at the click", () => {
    const shown = snapshotOf(shared());
    expect(notificationsStillAllowed(snapshotOf(shared()), shown, true, free)).toBe(true);
    expect(notificationsStillAllowed(snapshotOf(shared({ run_id: "r2" })), shown, true, free)).toBe(false);
    expect(notificationsStillAllowed(snapshotOf(shared({ notification_revision: 3 })), shown, true, free)).toBe(false);
    expect(notificationsStillAllowed(snapshotOf(shared({ muted: true })), shown, true, free)).toBe(false);
    expect(notificationsStillAllowed(snapshotOf(shared()), shown, false, free)).toBe(false);
  });
});

describe("Start fresh", () => {
  const stopped = (extra: Partial<ViewRow> = {}) => row({ state: "stopped", tracked: true, ...extra });
  it("is enabled for an online tracked paused or stopped codex, claude or kimi session", () => {
    for (const state of ["paused", "stopped"]) for (const agent of ["codex", "claude", "kimi"]) {
      expect(canStartStopped(row({ state, agent, tracked: true }), true, false)).toBe(true);
    }
  });
  it("is off when offline, untracked, live, archived, another agent or a write is pending", () => {
    expect(canStartStopped(stopped(), false, false)).toBe(false);
    expect(canStartStopped(stopped({ tracked: false }), true, false)).toBe(false);
    expect(canStartStopped(row({ tracked: true }), true, false)).toBe(false);
    expect(canStartStopped(stopped({ state: "archived" }), true, false)).toBe(false);
    expect(canStartStopped(stopped({ agent: "opencode" }), true, false)).toBe(false);
    expect(canStartStopped(stopped({ agent: "deepseek" }), true, false)).toBe(false);
    expect(canStartStopped(stopped(), true, true)).toBe(false);
    expect(canStartStopped(stopped(), true, false, true)).toBe(false);
  });
  it("shows the entry for paused or stopped rows only, with Qt's hint", () => {
    expect(freshEntry(stopped(), true, false)).toEqual({ enabled: true, hint: "Start with a new conversation and the same folder, account and model" });
    expect(freshEntry(row({ state: "paused", tracked: true }), true, false)?.enabled).toBe(true);
    expect(freshEntry(stopped(), false, false)?.enabled).toBe(false);
    expect(freshEntry(row(), true, false)).toBeUndefined();
    expect(freshEntry(stopped({ state: "archived" }), true, false)).toBeUndefined();
  });
  it("words the dialog as Qt does", () => {
    expect(freshCopy(stopped())).toEqual({
      title: "Start fresh?",
      text: "Start claude/proj/a on box with a new conversation?\n\nThe agent starts in the same folder with the same account and model, without the current conversation context. Earlier messages stay readable in Activity and in the agent\u2019s native history, but Resume will no longer continue them.",
      confirm: "Start fresh",
    });
  });
  it("refuses a changed row at confirmation", () => {
    const shown = snapshotOf(stopped());
    expect(freshStillValid(snapshotOf(stopped()), shown, true, false)).toBe(true);
    expect(freshStillValid(snapshotOf(stopped({ run_id: "r2" })), shown, true, false)).toBe(false);
    expect(freshStillValid(snapshotOf(stopped({ conversation_id: "c2" })), shown, true, false)).toBe(false);
    expect(freshStillValid(snapshotOf(row({ tracked: true })), shown, true, false)).toBe(false);
    expect(freshStillValid(snapshotOf(stopped()), shown, false, false)).toBe(false);
    expect(freshStillValid(snapshotOf(stopped()), shown, true, true)).toBe(false);
  });
  it("opens as a dialog kind", () => {
    const a = snapshotOf(stopped());
    expect(dialogReducer(undefined, { type: "open", kind: "fresh", row: a })).toEqual({ kind: "fresh", row: a });
  });
});

describe("header Start fresh slot", () => {
  it("follows Qt: main context only", () => {
    expect(headerClearSlot(true, "stopped")).toBe("fresh");
    expect(headerClearSlot(true, "paused")).toBe("fresh");
    expect(headerClearSlot(true, "running")).toBe("clear");
    expect(headerClearSlot(false, "stopped")).toBeUndefined();
    expect(headerClearSlot(false, "running")).toBeUndefined();
  });
  it("shows neither for an archived row", () => {
    expect(headerClearSlot(true, "archived")).toBeUndefined();
  });
});

describe("forget polish", () => {
  it("reads the row's forget reason when details have none", () => {
    const r = row({ state: "paused", allowed_actions: ["rename"], action_reasons: { forget: "Row says no" } });
    expect(r.forgetReason).toBe("Row says no");
    expect(menuState(r, true, false).end.reason).toBe("Row says no");
    expect(menuState(r, true, false, details({ allowed_actions: ["rename"], action_reasons: { forget: "Detail" } })).end.reason).toBe("Detail");
  });
  it("refuses at click time when running-ness changed, else binds the dialog to the live row", () => {
    const menu = snapshotOf(row({ state: "paused" }));
    expect(forgetOpening(menu, snapshotOf(row({ state: "running" })))).toBe("changed");
    const live = snapshotOf(row({ state: "paused", run_id: "r2", conversation_id: "c2" }));
    expect(forgetOpening(menu, live)).toMatchObject({ row: { runId: "r2" }, bound: { binding: false, run: "r2", conversation: "c2" } });
    const oc = snapshotOf(row({ agent: "opencode", state: "running", allowed_actions: ["forget", "rename"], run_id: "r3" }));
    expect(forgetOpening(oc, oc, details({ run_id: "d1", conversation_id: "dc" }))).toMatchObject({ bound: { binding: true, run: "d1", conversation: "dc" } });
  });
  it("requires a scope for a forget dialog", () => {
    const a = snapshotOf(row({ state: "paused" }));
    const bound = { binding: false, run: "r1", conversation: "c1" };
    expect(dialogReducer(undefined, { type: "open", kind: "forget", row: a, bound })).toEqual({ kind: "forget", row: a, bound });
    // @ts-expect-error a forget without its scope does not type-check
    dialogReducer(undefined, { type: "open", kind: "forget", row: a });
  });
});

describe("forgetCopy", () => {
  const saved = snapshotOf(row({ state: "paused" }));
  it("words the saved session", () => {
    expect(forgetCopy(saved, false)).toEqual({
      title: "Forget saved session?", text: "Remove claude/proj/a from HGS? The agent’s native conversation history is kept.",
      detail: "Computer: box", confirm: "Forget saved session",
    });
  });
  it("words the OpenCode binding", () => {
    expect(forgetCopy(saved, true)).toEqual({
      title: "Forget saved session?", text: "Forget claude/proj/a in Zerus? The conversation stays in OpenCode; only Zerus forgets it.",
      detail: "Computer: box", confirm: "Forget saved session",
    });
  });
  it("words the archived instance (Qt terminateSession 5771-5774, 5788-5789)", () => {
    const archived = snapshotOf(row({ state: "archived", archive_id: "a1" }));
    expect(forgetArchivedCopy(archived)).toEqual({
      title: "Forget archived session?",
      text: "Remove this archived instance of claude/proj/a from HGS? Other sessions with this name and the agent\u2019s native conversation history are kept.",
      detail: "Computer: box", confirm: "Forget archived session",
    });
  });
  it("sees a running OpenCode forget-only session as a binding", () => {
    expect(forgetsBindingOnly({ cmd: "opencode", allowed: "forget rename" })).toBe(true);
  });
  it("opens the forget dialog with its scope", () => {
    const bound = { binding: true, run: "r1", conversation: "c1" };
    expect(dialogReducer(undefined, { type: "open", kind: "forget", row: saved, bound })).toEqual({ kind: "forget", row: saved, bound });
  });
});

describe("archiveNameOccupied (Qt SessionsWindow.cpp 3148-3154)", () => {
  const live = (name: string, computer = "", state = "paused") => ({ key: `${computer}\nrun:${name}`, computer, name, state });
  const archived = { computer: "", name: "claude/proj/a" };
  it("is a live row of the same computer with the same name; archived rows and other computers do not count", () => {
    const rows = { a: live("claude/proj/a"), b: live("claude/proj/b") };
    expect(archiveNameOccupied(archived, rows)).toBe(true);
    expect(archiveNameOccupied(archived, { a: live("claude/proj/a", "laptop"), b: live("claude/proj/b") })).toBe(false);
    expect(archiveNameOccupied(archived, { a: live("claude/proj/a", "", "archived") })).toBe(false);
    expect(archiveNameOccupied(archived, { a: live("claude/proj/a", "", "running") })).toBe(true);
    expect(archiveNameOccupied(archived, {})).toBe(false);
  });
  it("gives the same answer from a name index built once", () => {
    const rows = { a: live("claude/proj/a"), b: live("claude/proj/b", "laptop"), c: live("claude/proj/c", "", "archived") };
    const index = liveNameIndex(rows);
    expect([...index].sort()).toEqual([nameKey({ computer: "", name: "claude/proj/a" }), nameKey({ computer: "laptop", name: "claude/proj/b" })].sort());
    for (const s of [archived, { computer: "laptop", name: "claude/proj/b" }, { computer: "", name: "claude/proj/b" }, { computer: "", name: "claude/proj/c" }]) {
      expect(index.has(nameKey(s))).toBe(archiveNameOccupied(s, rows));
    }
  });
});
