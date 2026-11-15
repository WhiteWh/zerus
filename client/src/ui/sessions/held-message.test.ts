import { describe, expect, it } from "vitest";
import { heldInputs, heldMessage, simplified, terminalDraftBar, withoutHeld } from "./held-message";
import { buildTranscript } from "./transcript";
import type { LocalMessage } from "../../types";
import type { Session } from "./types";

const session = { key: "\nrun:run", catalogKey: "run:run", computer: "", name: "claude/example/review", runId: "run", conversationId: "conversation",
  sessionId: "11111111-1111-4111-8111-111111111111" } as Session;
const held = { request_id: "send-1", text: "Please continue with the migration", attachments: ["plan.md"] };
const row = (id: string, text: string): LocalMessage => ({ id, text, status: "sending", attachments: [], run_id: "run", conversation_id: "conversation",
  submitted_at: 1_793_000_000, error: "", uncertain: false, message_id: null, submitted_text: null, source: null, voice: null });

describe("a Compact and continue message held while the agent compacts (zerus-pabah)", () => {
  it("follows the open composer's own view, never a draft-key comparison (zerus-i0ak2)", () => {
    const main = { subagent: null };
    expect(heldMessage(held, main, session, false)).toEqual(held);
    expect(heldMessage(null, main, session, false)).toBeUndefined();
    expect(heldMessage(held, undefined, session, false)).toBeUndefined();
    expect(heldMessage(held, main, undefined, false)).toBeUndefined();
    // A row whose key would differ from the view's still shows the composer's own held message.
    expect(heldMessage(held, main, { ...session, sessionId: "00000000-0000-0000-0000-000000000000" } as Session, false)).toEqual(held);
    // Never a child's composer, and never for an archived row.
    expect(heldMessage(held, { subagent: { agent_id: "child", send_supported: true, hint: "" } }, session, false)).toBeUndefined();
    expect(heldMessage(held, main, session, true)).toBeUndefined();
    expect(heldMessage(held, main, { ...session, state: "archived", archiveId: "a1" } as Session, false)).toBeUndefined();
  });

  it("stands for its own Sending row, and only that row", () => {
    const rows = [row("send-1", held.text), row("other", "An earlier message")];
    expect(withoutHeld(rows, held).map((r) => r.id)).toEqual(["other"]);
    expect(withoutHeld(rows, null)).toBe(rows);
    // The transcript then shows the held text once: in the held entry, not as "Sending…".
    const shown = JSON.stringify(buildTranscript([], {}, withoutHeld(rows, held)));
    expect(shown).not.toContain(held.text);
    expect(shown).toContain("An earlier message");
  });
});

describe("messages hgs holds until the agent takes the previous one (U35)", () => {
  const rows = [{ request_id: "11111111-2222-4333-8444-555555555555", text: "first\n\n  of two", cancelling: false },
    { request_id: "11111111-2222-4333-8444-666666666666", text: "second", cancelling: true }];
  const main = { subagent: null };

  it("shows on a main composer of a live session only, oldest first", () => {
    expect(heldInputs(rows, main, session, false)).toBe(rows);
    expect(heldInputs([], main, session, false)).toEqual([]);
    expect(heldInputs(undefined, main, session, false)).toEqual([]);
    expect(heldInputs(rows, { subagent: { agent_id: "child", send_supported: true, hint: "" } }, session, false)).toEqual([]);
    expect(heldInputs(rows, main, session, true)).toEqual([]);
    expect(heldInputs(rows, main, { ...session, state: "archived", archiveId: "a1" } as Session, false)).toEqual([]);
    expect(heldInputs(rows, undefined, session, false)).toEqual([]);
    // One empty value, so a memo over it does not change while nothing is held.
    expect(heldInputs(undefined, main, session, false)).toBe(heldInputs([], main, undefined, false));
  });

  it("previews a message on one line, as Qt's simplified text", () => {
    expect(simplified(rows[0].text)).toBe("first of two");
    expect(simplified("  \t tabbed\r\nlines  ")).toBe("tabbed lines");
    expect(simplified("")).toBe("");
  });

  it("shows the Terminal draft bar under the same rule", () => {
    const bar = { draft: "half a thought", can_send: true, submitting: false, error: null };
    expect(terminalDraftBar(bar, main, session, false)).toBe(bar);
    expect(terminalDraftBar(null, main, session, false)).toBeUndefined();
    expect(terminalDraftBar(bar, main, session, true)).toBeUndefined();
    expect(terminalDraftBar(bar, { subagent: { agent_id: "child", send_supported: true, hint: "" } }, session, false)).toBeUndefined();
    expect(terminalDraftBar(bar, main, { ...session, state: "archived", archiveId: "a1" } as Session, false)).toBeUndefined();
  });
});
