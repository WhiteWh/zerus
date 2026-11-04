import { describe, expect, it } from "vitest";
import type { LocalMessage, QueueView } from "../../types";
import type { Session } from "./types";
import { cardTarget } from "./attachment-card";
import { queueEntries, queuePresentation, queuedMessages, sanitizedQueueText } from "./queue-message";

const text = "**Review** [Image #1]";
const submitted = '**Review** [Image #1] (Inspect the attached image at "/synthetic/attachments/message-one/01-shot.png")';
const file = { request_id: "receipt-one", index: 0, name: "shot.png", mime: "image/png", bytes: 1536, reference: "[Image #1]" };
const queue = (patch: Partial<QueueView> = {}): QueueView => ({ id: "native-screen-id", text: `❯ ${submitted}`,
  hint: "send now", enabled: true, can_send_now: true, label: "Send now", run_id: "run", conversation_id: "conversation", ...patch });
const local = (patch: Partial<LocalMessage> = {}): LocalMessage => ({ id: "receipt-one", text, status: "sent", attachments: [file],
  run_id: "run", conversation_id: "conversation", submitted_at: 1, error: "", uncertain: false,
  message_id: "receipt-one", submitted_text: submitted, source: null, voice: null, ...patch });
const summary = (patch: Record<string, unknown> = {}) => ({ run_id: "run", conversation_id: "conversation",
  attachment_messages: [{ source: "hgs_delivery", agent_id: "", message_id: "receipt-one", detail: text, submitted_text: submitted, attachments: [file] }], ...patch });

describe("native queued outgoing presentation", () => {
  it("removes native chrome while preserving Markdown and separate queued entries", () => {
    expect(queueEntries("❯ **one**\n  - item\n❯ `two`")).toEqual([
      { text: "**one**\n- item", exact: true }, { text: "`two`", exact: true },
    ]);
    expect(queueEntries("\u2022 Messages to be submitted after next tool call (press esc to\n  interrupt and send immediately)\n  ↳ **one**\n    continuation\n  ↳ two\nshift+← edit last queued message")).toEqual([
      { text: "**one**\ncontinuation", exact: true }, { text: "two", exact: true },
    ]);
    expect(queueEntries("↳ one\n  ↳ two").map(e => e.text)).toEqual(["one", "two"]);
    expect(queueEntries("\u001b[32m❯ **green**\u001b[0m")[0].text).toBe("**green**");
    expect(queueEntries("foreign first line\nsecond line")).toEqual([{ text: "foreign first line\nsecond line", exact: false }]);
  });

  it("uses an exact local send receipt for the original body and guarded attachment card", () => {
    const [item] = queuedMessages(queue(), [local()]);
    expect(item.body).toBe(text);
    expect(item.kind).toBe("user");
    expect(item.local).toBeUndefined(); // no sent/retry/delete actions for a native pending message
    expect(item.attachments?.[0]).toMatchObject({ name: "shot.png", request_id: "receipt-one", index: 0, image: true });
    expect(cardTarget(item.attachments![0], { computer: "", name: "codex/example/queue", conversation_id: "conversation", archive_id: null })?.ref.request_id).toBe("receipt-one");
    expect(JSON.stringify(item)).not.toContain("/synthetic/");
    expect(queue().id).toBe("native-screen-id"); // display reconciliation does not replace the Send now identity
  });

  it("keeps display identity independent of the screen hash while Send now retains its native ID", () => {
    const before = queue(), after = queue({ id: "screen-after-an-unrelated-native-spinner" });
    expect(queuedMessages(before, [local()])).toEqual(queuedMessages(after, [local()]));
    expect(before.id).not.toBe(after.id);
  });

  it("uses an exact host receipt and deduplicates the same receipt during local-to-host overlap", () => {
    expect(queuedMessages(queue(), [], summary())[0].body).toBe(text);
    expect(queuedMessages(queue(), [local()], summary())[0].attachments?.[0].request_id).toBe("receipt-one");
    const duplicate = summary();
    duplicate.attachment_messages.push({ ...duplicate.attachment_messages[0], message_id: "receipt-two", attachments: [{ ...file, request_id: "receipt-two" }] });
    expect(queuedMessages(queue(), [local()], duplicate)[0].attachments).toBeUndefined();
  });

  it("never matches a prefix, shortened entry, other run, uncertain send or duplicate queue body", () => {
    for (const [q, rows] of [
      [queue({ text: `❯ ${submitted.slice(0, 25)}` }), [local()]],
      [queue({ text: `❯ ${submitted}…` }), [local({ submitted_text: submitted + "…" })]],
      [queue(), [local({ run_id: "old" })]],
      [queue(), [local({ conversation_id: "old" })]],
      [queue(), [local({ uncertain: true })]],
      [queue(), [local({ status: "sending" })]],
      [queue({ text: `❯ ${submitted}\n❯ ${submitted}` }), [local()]],
      [queue({ text: `❯ ${submitted}\n  ❯ continuation resembling a prompt` }), [local()]],
    ] as const) {
      expect(queuedMessages(q, rows).every(item => item.attachments === undefined)).toBe(true);
    }
    expect(queuedMessages(queue(), [], summary({ conversation_id: "old" }))[0].attachments).toBeUndefined();
    expect(queuedMessages(queue({ run_id: undefined }), [local()])[0].attachments).toBeUndefined();
  });

  it("sanitizes unmatched attachment instructions without adopting or reading the host path", () => {
    const [item] = queuedMessages(queue(), []);
    expect(item.body).toBe(text);
    expect(item.attachments).toBeUndefined();
    expect(sanitizedQueueText('See [File #1] (Read the attached file at "/synthetic/note.txt")')).toBe("See [File #1]");
    expect(sanitizedQueueText('[File #1] Read the attached file at "/synthetic/note.txt"')).toBe("[File #1]");
    expect(sanitizedQueueText('Inspect the attached image at "/synthetic/shot.png"')).toBe("[Image]");
    expect(sanitizedQueueText('[Image #1] (Inspect the attached image at /synthetic/shot.png)')).toBe("[Image #1]");
  });

  it("refuses malformed or conflicting public file metadata", () => {
    for (const attachments of [[{ ...file, request_id: "other" }], [{ ...file, index: 4 }], [{ ...file, bytes: NaN }]]) {
      expect(queuedMessages(queue(), [local({ attachments })])[0].attachments).toBeUndefined();
    }
    const conflict = summary(); conflict.attachment_messages[0].attachments = [{ ...file, bytes: file.bytes + 1 }];
    expect(queuedMessages(queue(), [local()], conflict)[0].attachments).toBeUndefined();
  });
});

describe("native queue receipt source fences", () => {
  const session = { key: "\nrun:run", catalogKey: "run:run", computer: "", name: "codex/example/queue", runId: "run", conversationId: "conversation" } as Session;
  const view = { key: "\ncodex/example/queue", local: [local()] };
  const target = { id: session.key, key: session.catalogKey, computer: "", name: session.name };
  it("binds the main selected composer and its current Activity before supplying attachment scope", () => {
    const shown = queuePresentation(queue(), session, view, target, summary(), false);
    expect(shown.items[0].attachments?.[0].request_id).toBe("receipt-one");
    expect(shown.scope).toEqual({ computer: "", name: session.name, conversation_id: "conversation", archive_id: null });
    for (const [row, composer, child] of [
      [{ ...session, runId: "old" }, view, false],
      [{ ...session, conversationId: "old" }, view, false],
      [{ ...session, computer: "peer" }, view, false],
      [session, { ...view, key: "another" }, false],
      [session, { ...view, subagent: { agent_id: "child", send_supported: true, hint: "" } }, false],
      [session, view, true],
    ] as const) {
      const shown = queuePresentation(queue(), row, composer, target, summary(), child);
      expect(shown.scope).toBeUndefined();
      expect(shown.items[0].attachments).toBeUndefined();
    }
  });
  it("does not use a late host summary from a different follow, while local receipts remain independent", () => {
    const stale = { ...target, id: "other" };
    expect(queuePresentation(queue(), session, { ...view, local: [] }, stale, summary(), false).items[0].attachments).toBeUndefined();
    expect(queuePresentation(queue(), session, view, stale, summary(), false).items[0].attachments?.[0].request_id).toBe("receipt-one");
  });
});
