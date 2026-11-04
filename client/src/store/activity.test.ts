import { describe, expect, it, vi } from "vitest";
import { createRoot } from "solid-js";
import { createActivity, type FollowRequest } from "./activity";
import { buildTranscript, mergeProvider } from "../ui/sessions/transcript";
import type { ActivityEvent, ActivityOut } from "../types";

const target = { id: "\nrun:a", computer: "", key: "run:a", name: "claude/sample-project/a" };
const ev = (seq: number, type = "PostToolUse", extra: Partial<ActivityEvent> = {}): ActivityEvent => ({ seq, type, source: "claude_hooks", tool: "Bash", detail: `step ${seq}`, at: 1_793_000_000 + seq, ...extra });
const out = (kind: string, payload: unknown, key = "run:a"): ActivityOut => ({ computer: "", key, kind, payload });

function setup() {
  const calls: FollowRequest[] = [];
  const unfollows: string[] = [];
  const port = { follow: vi.fn(async (r: FollowRequest) => { calls.push(r); }), unfollow: vi.fn(async (_c: string, k: string) => { unfollows.push(k); }) };
  const store = createActivity(port);
  return { store, calls, unfollows };
}

describe("activity store", () => {
  it("changes routing immediately but waits for the native composer before subscribing", async () => {
    const { store, calls } = setup();
    store.follow({ ...target, conversationId: "conversation" });
    store.applyActivity([out("events", { events: [ev(10)], cursor: 10 })]);
    let opened!: () => void;
    const ready = new Promise<void>((resolve) => { opened = resolve; });
    const next = { ...target, id: "\nrun:b", key: "run:b", conversationId: "conversation" };
    store.follow(next, ready);
    expect(store.target()).toBe(next);
    expect(store.events().map(e => e.seq)).toEqual([10]);
    expect(store.summary()).toBeUndefined();
    expect(calls.map(call => call.key)).toEqual(["run:a"]);
    store.loadOlder();
    expect(calls).toHaveLength(1);
    opened();
    await ready;
    expect(calls.map(call => call.key)).toEqual(["run:a", "run:b"]);
    expect(calls[1]).toMatchObject({ after: 10, tail: 100 });
    store.follow(undefined);
  });

  it("never subscribes an obsolete target whose composer finishes opening late", async () => {
    const { store, calls } = setup();
    let opened!: () => void;
    const ready = new Promise<void>((resolve) => { opened = resolve; });
    store.follow(target, ready);
    const next = { ...target, id: "\nrun:b", key: "run:b" };
    store.follow(next, Promise.resolve());
    await Promise.resolve();
    opened();
    await ready;
    expect(calls.map(call => call.key)).toEqual(["run:b"]);
    store.follow(undefined);
  });

  it("still reads Activity after a composer open fails, without retrying the open", async () => {
    const { store, calls } = setup();
    const failed = Promise.reject(new Error("native open refused"));
    store.follow(target, failed);
    await failed.catch(() => {});
    expect(calls.map(call => call.key)).toEqual(["run:a"]);
    store.follow(undefined);
  });

  it("retains loaded history and its display key while the same conversation resumes", () => {
    createRoot((dispose) => {
      const { store, calls, unfollows } = setup();
      store.follow({ ...target, conversationId: "conversation" });
      const history = Array.from({ length: 100 }, (_, n) => ev(n + 101));
      store.applyActivity([out("events", { events: history, cursor: 200 }), out("state", { value: { conversation_id: "conversation" }, rev: 9 })]);
      const shown = store.events();
      store.follow({ ...target, id: "\nrun:b", key: "run:b", runId: "b", conversationId: "conversation" });
      expect(store.events()).toBe(shown);
      expect(store.loaded()).toBe(true);
      expect(store.displayKey()).toBe(target.id);
      expect(store.summary()).toBeUndefined();
      expect(unfollows).toEqual(["run:a"]);
      expect(calls.at(-1)).toMatchObject({ key: "run:b", run_id: "b", after: 200, tail: 100 });
      store.applyActivity([out("events", { events: [ev(999)] }), out("events", { events: [ev(201)], cursor: 201 }, "run:b"),
        out("state", { value: { conversation_id: "conversation" }, rev: 1 }, "run:b")]);
      expect(store.events().map(e => e.seq)).toEqual([...history.map(e => e.seq), 201]);
      store.loadOlder();
      expect(calls.at(-1)).toMatchObject({ key: "run:b", run_id: "b", after: 0, tail: 100 });
      dispose();
    });
  });

  it("keeps the old history through an empty startup window but drops it on a different native conversation", () => {
    createRoot((dispose) => {
      const { store } = setup();
      store.follow({ ...target, conversationId: "conversation" });
      store.applyActivity([out("events", { events: [ev(10)], cursor: 10 })]);
      store.follow({ ...target, id: "\nrun:b", key: "run:b", conversationId: "conversation" });
      store.applyActivity([out("replace", { events: [] }, "run:b")]);
      expect(store.events().map(e => e.seq)).toEqual([10]);
      store.applyActivity([out("events", { events: [ev(20)] }, "run:b"),
        out("state", { value: { conversation_id: "different" }, rev: 1 }, "run:b")]);
      expect(store.events().map(e => e.seq)).toEqual([20]);
      expect(store.displayKey()).not.toBe(target.id);
      dispose();
    });
  });

  it("retains provider history for display without exposing the old run as current evidence", () => {
    createRoot((dispose) => {
      const { store } = setup();
      const previous = { run_id: "a", conversation_id: "conversation", provider_messages: [{ message_id: "reply", detail: "Full historical reply" }] };
      store.follow({ ...target, conversationId: "conversation" });
      store.applyActivity([out("state", { value: previous, rev: 9 })]);
      store.follow({ ...target, id: "\nrun:b", key: "run:b", conversationId: "conversation" });
      expect(store.summary()).toBeUndefined();
      expect(store.transcriptSummary()).toEqual(previous);
      const current = { run_id: "b", conversation_id: "conversation", phase: "starting" };
      store.applyActivity([out("state", { value: current, rev: 1 }, "run:b")]);
      expect(store.summary()).toEqual(current);
      expect(store.transcriptSummary()).toEqual(previous);
      store.applyActivity([out("events", { events: [], cursor: 0 }, "run:b")]);
      expect(store.transcriptSummary()).toEqual(current);
      dispose();
    });
  });

  it("does not promote an unconfirmed intermediate run's page or cursor on another resume", () => {
    createRoot((dispose) => {
      const { store, calls } = setup();
      store.follow({ ...target, conversationId: "conversation" });
      store.applyActivity([out("events", { events: [ev(10)], cursor: 10 })]);
      store.follow({ ...target, id: "\nrun:b", key: "run:b", conversationId: "conversation" });
      store.applyActivity([out("events", { events: [ev(20)] }, "run:b")]);
      store.follow({ ...target, id: "\nrun:c", key: "run:c", conversationId: "conversation" });
      expect(store.events().map(e => e.seq)).toEqual([10]);
      expect(calls.at(-1)?.after).toBe(10);
      store.applyActivity([out("events", { events: [ev(20), ev(30)] }, "run:c"),
        out("state", { value: { conversation_id: "conversation" }, rev: 1 }, "run:c")]);
      expect(store.events().map(e => e.seq)).toEqual([10, 20, 30]);
      dispose();
    });
  });

  it.each([{ computer: "peer" }, { name: "claude/sample-project/other" }, { conversationId: "different" }, { conversationId: "" }])(
    "clears history instead of borrowing another or unknown conversation: %j", (changed) => createRoot((dispose) => {
      const { store } = setup();
      store.follow({ ...target, conversationId: "conversation" });
      store.applyActivity([out("events", { events: [ev(10)], cursor: 10 })]);
      store.follow({ ...target, id: "new", key: "run:b", conversationId: "conversation", ...changed });
      expect(store.events()).toEqual([]);
      expect(store.loaded()).toBe(false);
      dispose();
    }));

  it("retains last-known activity across service loss and accepts restarted native revisions", () => {
    createRoot((dispose) => {
      const { store } = setup();
      store.follow(target);
      store.applyActivity([out("replace", { events: [ev(100)], cursor: 100 }),
        out("state", { value: { message: "last known" }, rev: 90 })]);
      store.loadOlder();
      expect(store.loadingOlder()).toBe(true);
      store.applyActivity([out("connection", { ready: false, code: "source_offline" })]);
      expect(store.error()).toBeTruthy();
      expect(store.loaded()).toBe(true);
      expect(store.loadingOlder()).toBe(false);
      expect(store.olderDone()).toBe(false);
      expect(store.ended()).toBe(false);
      expect(store.events().map(e => e.seq)).toEqual([100]);
      expect(store.summary()).toEqual({ message: "last known" });
      store.applyActivity([out("connection", { ready: true, code: "subscribed" }),
        out("state", { value: { message: "new source" }, rev: 1 })]);
      expect(store.error()).toBeUndefined();
      expect(store.summary()).toEqual({ message: "new source" });
      expect(store.events().map(e => e.seq)).toEqual([100]);
      dispose();
    });
  });

  it("keeps an older read pending through the service subscription fence", () => {
    createRoot((dispose) => {
      const { store } = setup();
      store.follow(target);
      store.applyActivity([out("replace", { events: [ev(100)], cursor: 100 })]);
      store.loadOlder();
      store.applyActivity([out("connection", { ready: false, pending: true, code: "awaiting_fence" }),
        out("connection", { ready: true, pending: false, code: "subscribed" })]);
      expect(store.loadingOlder()).toBe(true);
      expect(store.error()).toBeUndefined();
      expect(store.olderDone()).toBe(false);
      store.applyActivity([out("events", { events: [ev(1)], cursor: 100 })]);
      expect(store.loadingOlder()).toBe(false);
      expect(store.events().map(e => e.seq)).toEqual([1, 100]);
      dispose();
    });
  });

  it("follows with the tail, merges events by seq and ignores other sessions", () => {
    createRoot((dispose) => {
      const { store, calls } = setup();
      store.follow(target);
      expect(calls).toEqual([{ computer: "", key: "run:a", name: target.name, run_id: "a", conversation_id: undefined, after: undefined, tail: 100 }]);
      store.applyActivity([out("events", { events: [ev(5), ev(7)], cursor: 7 })]);
      store.applyActivity([out("events", { events: [ev(7), ev(9)], cursor: 9 }), out("events", { events: [ev(1)], cursor: 1 }, "run:other")]);
      expect(store.events().map((e) => e.seq)).toEqual([5, 7, 9]);
      expect(store.loaded()).toBe(true);
      dispose();
    });
  });

  it("reads its attachments in the inspected conversation, else the row's", () => {
    createRoot((dispose) => {
      const { store } = setup();
      expect(store.attachmentScope()).toBeUndefined();
      store.follow({ ...target, conversationId: "conv-row" });
      expect(store.attachmentScope()).toEqual({ computer: "", name: target.name, conversation_id: "conv-row", archive_id: null });
      store.applyActivity([out("state", { value: { conversation_id: "conv-inspected" } })]);
      expect(store.attachmentScope()?.conversation_id).toBe("conv-inspected");
      dispose();
    });
  });

  it("reads older pages with `after` and doubles the reach after an empty read", () => {
    createRoot((dispose) => {
      const { store, calls } = setup();
      store.follow(target);
      store.applyActivity([out("events", { events: [ev(900), ev(950)], cursor: 950 })]);

      store.loadOlder();
      expect(calls.at(-1)?.after).toBe(700);
      expect(store.loadingOlder()).toBe(true);
      store.applyActivity([out("events", { events: [ev(750), ev(900), ev(950)], cursor: 950 })]);
      expect(store.loadingOlder()).toBe(false);
      expect(store.events()[0].seq).toBe(750);

      // Nothing older within 200 rows: the next read reaches 400 back.
      store.loadOlder();
      expect(calls.at(-1)?.after).toBe(550);
      store.applyActivity([out("events", { events: [ev(750)], cursor: 950 })]);
      store.loadOlder();
      expect(calls.at(-1)?.after).toBe(350);
      dispose();
    });
  });

  it("stops reading older pages once a read from the start brings nothing", () => {
    createRoot((dispose) => {
      const { store, calls } = setup();
      store.follow(target);
      store.applyActivity([out("events", { events: [ev(120)], cursor: 120 })]);
      store.loadOlder();
      expect(calls.at(-1)?.after).toBe(0);
      store.applyActivity([out("events", { events: [ev(120)], cursor: 120 })]);
      expect(store.olderDone()).toBe(true);
      const before = calls.length;
      store.loadOlder();
      expect(calls.length).toBe(before);
      dispose();
    });
  });

  it("replaces the window, keeps the newest state and marks the end", () => {
    createRoot((dispose) => {
      const { store } = setup();
      store.follow(target);
      store.applyActivity([out("events", { events: [ev(1), ev(2)], cursor: 2 })]);
      store.applyActivity([out("replace", { events: [ev(10)], cursor: 10 }), out("state", { value: { phase: "tool" }, rev: 3 }), out("state", { value: { phase: "idle" }, rev: 2 })]);
      expect(store.events().map((e) => e.seq)).toEqual([10]);
      expect(store.summary()).toEqual({ phase: "tool" });
      store.applyActivity([out("ended", null)]);
      expect(store.ended()).toBe(true);
      dispose();
    });
  });

  it("unfollows the previous session when the selection changes", () => {
    createRoot((dispose) => {
      const { store, unfollows } = setup();
      store.follow(target);
      store.applyActivity([out("events", { events: [ev(1)], cursor: 1 })]);
      store.follow({ ...target, id: "\nrun:b", key: "run:b" });
      expect(unfollows).toEqual(["run:a"]);
      expect(store.events()).toEqual([]);
      dispose();
    });
  });
});

describe("transcript", () => {
  it("builds messages, receipts and tool groups as the Qt Activity does", () => {
    const items = buildTranscript([
      ev(1, "UserPromptSubmit", { detail: "Fix the cursor" }),
      ev(2, "PreToolUse", { tool: "Read" }),
      ev(3, "PostToolUse", { tool: "Read" }),
      ev(4, "PreToolUse", { tool: "Bash" }),
      ev(5, "PostToolUseFailure", { tool: "Bash", detail: "exit status 1" }),
      ev(6, "AgentThinking", { detail: "The empty page needs a branch." }),
      ev(7, "Stop", { detail: "Fixed." }),
      ev(8, "UserPromptQueued", { detail: "Also run the docs check" }),
      ev(9, "UserPromptSubmit", { origin: "peer_message", sender: "codex/p/review", detail: "Rebase first" }),
    ]);
    expect(items.map((i) => i.kind)).toEqual(["user", "tools", "tool", "thinking", "agent", "user", "system"]);
    const [user, tools, failure] = items as { title?: string; body?: string; receipt?: string; warn?: boolean }[];
    expect(user.receipt).toBe("Sent");
    expect(tools.title).toBe("2 tool calls, 3 events");
    expect(tools.body).toBe("Read, Bash");
    expect(failure.title).toBe("Tool failed / Bash");
    expect(failure.warn).toBe(true);
    expect((items[5] as { receipt?: string }).receipt).toBe("Queued at this time");
    expect((items[6] as { sender?: string }).sender).toBe("Message from codex/p/review");
  });
});

describe("provider messages in Activity", () => {
  const body = (item: ReturnType<typeof buildTranscript>[number]) => "body" in item ? item.body : undefined;
  const message = (id: string, detail: string, at: number, phase = "commentary"): ActivityEvent => ({
    type: "AgentMessage", source: "codex_transcript", message_id: id, detail, at, message_phase: phase,
  });

  it("shows snapshot commentary between tool outputs while the turn is still working", () => {
    createRoot((dispose) => {
      const { store } = setup();
      store.follow(target);
      store.applyActivity([
        out("events", { events: [ev(10), ev(12, "PreToolUse")], cursor: 12 }),
        out("state", { rev: 1, value: { activity: "busy", provider_messages: [message("mid-turn", "The tool found the cause.", 1_793_000_011)] } }),
      ]);
      const items = buildTranscript(store.events(), store.summary());
      expect(items.map((i) => i.kind)).toEqual(["tool", "agent", "tool"]);
      expect(body(items[1])).toBe("The tool found the cause.");
      // Provider rows do not become fake journal cursors or alter older paging.
      expect(store.events().map((e) => e.seq)).toEqual([10, 12]);
      dispose();
    });
  });

  it("shows initial and streamed provider messages once as the core updates the summary at the same rev", () => {
    createRoot((dispose) => {
      const { store } = setup();
      store.follow(target);
      const progress = message("progress", "Checking the output.", 10);
      const complete = message("complete", "Fixed.", 20, "final_answer");
      store.applyActivity([out("state", { rev: 7, value: { provider_messages: [progress] } })]);
      expect(buildTranscript(store.events(), store.summary()).map(body)).toEqual(["Checking the output."]);
      // This is the core's ActivityUpdate::State for a streamed event frame:
      // it retains rev 7, replacing the message with its latest content.
      const latest = { ...progress, detail: "The output confirms the cause." };
      for (let i = 0; i < 2; i++) store.applyActivity([out("state", { rev: 7, value: { provider_messages: [latest, complete] } })]);
      expect(buildTranscript(store.events(), store.summary()).map(body)).toEqual(["The output confirms the cause.", "Fixed."]);
      dispose();
    });
  });

  it("merges the full final response into its Stop excerpt and preserves commentary", () => {
    const given = [ev(1, "Stop", { at: 20, detail: "Fixed the…" })];
    const items = buildTranscript(given, { provider_messages: [
      message("progress", "A separate progress update.", 19),
      message("final", "Fixed the missing message.", 20, "final_answer"),
    ] });
    expect(items.map(body)).toEqual(["A separate progress update.", "Fixed the missing message."]);
    expect(items[1].id).toBe("s1");
    expect(given[0].detail).toBe("Fixed the…");
    const emptyStop = buildTranscript([ev(1, "Stop", { at: 20, detail: "" })], {
      provider_messages: [message("progress", "Still working.", 19)],
    });
    expect(body(emptyStop[0])).toBe("Still working.");
    expect(emptyStop[0].kind).toBe("agent");
  });

  it("deduplicates a provider message already streamed as an event by native identity", () => {
    const progress = message("native-id", "Progress.", 10);
    const items = buildTranscript([progress], { provider_messages: [progress, progress] });
    expect(items.map(body)).toEqual(["Progress."]);
  });

  it("preserves distinct native messages even with identical text and timestamps", () => {
    const items = buildTranscript([], { provider_messages: [message("first", "Still working.", 10), message("second", "Still working.", 10)] });
    expect(items).toHaveLength(2);
    expect(items[0].id).not.toBe(items[1].id);
  });

  it("a Stop excerpt cannot absorb two distinct final messages", () => {
    const items = buildTranscript([ev(1, "Stop", { detail: "Done.", at: 20 })], {
      provider_messages: [message("first-final", "Done.", 20, "final_answer"), message("second-final", "Done.", 21, "final_answer")],
    });
    expect(items.map(body)).toEqual(["Done.", "Done."]);
    expect(items[0].id).not.toBe(items[1].id);
  });

  it("keeps an untimed journal event in sequence position when inserting provider rows", () => {
    const given = [ev(1, "AgentMessage", { at: 10 }), ev(2, "AgentThinking", { at: undefined }), ev(3, "AgentMessage", { at: 30 })];
    const merged = mergeProvider(given, { provider_messages: [message("progress", "Working.", 20)] });
    expect(merged.filter((e) => e.seq).map((e) => e.seq)).toEqual([1, 2, 3]);
    expect(merged[1]).toBe(given[1]);
  });

  it("preserves journal order when every provider message is already present or fills a Stop", () => {
    const native = message("already-there", "Progress.", 20);
    const given = [ev(1, "Stop", { at: 30, detail: "Done…" }), ev(2, "AgentThinking", { at: undefined }), native];
    expect(mergeProvider(given, { provider_messages: [native] })).toEqual(given);
    const filled = mergeProvider(given, { provider_messages: [message("final", "Done with checks.", 30, "final_answer")] });
    expect(filled.map((e) => e.seq)).toEqual(given.map((e) => e.seq));
    expect(filled[0].detail).toBe("Done with checks.");
  });

  it("breaks equal occurrence times by journal sequence", () => {
    const merged = mergeProvider([ev(3, "AgentThinking", { at: 10 }), ev(1, "AgentThinking", { at: 10 })], {
      provider_messages: [message("later", "Done.", 20)],
    });
    expect(merged.slice(0, 2).map((e) => e.seq)).toEqual([1, 3]);
  });

  it("indexes a long history once and only matches the nearby time window", () => {
    let reads = 0;
    const history = Array.from({ length: 2000 }, (_, i): ActivityEvent => ({
      type: "Stop", detail: "Old reply", seq: i + 1, get at() { reads++; return i + 1; },
    }));
    const provider_messages = Array.from({ length: 100 }, (_, i) => message(`recent-${i}`, "New reply", 10000 + i));
    expect(mergeProvider(history, { provider_messages })).toHaveLength(2100);
    expect(reads).toBeLessThan(20000);
  });

  it("only projects the bounded provider window from the current summary", () => {
    const items = buildTranscript([], { provider_messages: Array.from({ length: 130 }, (_, i) => message(`native-${i}`, `Update ${i}`, i + 1)) });
    expect(items).toHaveLength(100);
    expect(body(items[0])).toBe("Update 30");
  });
});

describe("question replies in the transcript", () => {
  it("answers are labelled and keep their questions", () => {
    const items = buildTranscript([
      { seq: 1, type: "QuestionAnswered", at: 10, detail: "Which checks? → Unit" },
      { seq: 2, type: "UserPromptSubmit", at: 11, detail: "<send_user_message_question_reply>…", question_replies: [{ question: "Which checks?", answer: "Unit" }] },
    ] as ActivityEvent[]);
    expect(items.map((i) => (i as { label?: string }).label)).toEqual(["You (answer)", "You (answer)"]);
    expect((items[1] as { replies?: unknown }).replies).toEqual([{ question: "Which checks?", answer: "Unit" }]);
  });

  it("a recorded prompt with answers reads as a recorded answer", () => {
    const items = buildTranscript([], { prompt: "<send_user_message_question_reply>…", prompt_replies: [{ question: "Q", answer: "A" }] });
    const prompt = items.find((i) => i.id === "prompt-snapshot") as { label?: string; replies?: unknown };
    expect(prompt.label).toBe("You (recorded answer)");
    expect(prompt.replies).toEqual([{ question: "Q", answer: "A" }]);
  });
});
