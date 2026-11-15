import { describe, expect, it } from "vitest";
import { buildTranscript, createActivityProjections, localFileAvailable, mergeLocal, createTranscriptProjection, toolPreview, TOOL_PREVIEW_SCAN } from "./transcript";
import type { ActivityEvent, EventAttachment, LocalMessage } from "../../types";
import type { ActivityItem, MediaMessage } from "./types";
import { cardTarget, rowScope } from "./attachment-card";

const local = (status: "sending" | "error" | "sent", uncertain = false, extra: Partial<LocalMessage> = {}): LocalMessage => ({
  id: `l-${status}-${uncertain}`, text: "hello", status, attachments: [], run_id: "r", conversation_id: "c",
  submitted_at: 1_793_000_000, error: status === "error" ? "hgs: refused" : "", uncertain, message_id: null, submitted_text: null, source: null, voice: null,
  ...extra,
});

describe("OpenCode inbox messages (ActivityView.cpp:1036-1038, 1072-1087; zerus-5wmh.13.6)", () => {
  const row = (delivery: string, message_id = "msg_queued01"): ActivityEvent =>
    ({ type: "UserPromptSubmit", at: 10, detail: "later", source: "opencode_native", message_id, delivery_state: delivery });
  const summary = { run_id: "run-1", conversation_id: "conv-1", allowed_actions: ["rename", "forget", "remove_queued"] };

  it("a queued message is stamped Queued and offers nothing", () => {
    const [item] = buildTranscript([row("queued")], summary) as ActivityItem[];
    expect(item).toMatchObject({ kind: "user", receipt: "Queued",
      inbox: { state: "queued", messageId: "msg_queued01", run: "run-1", conversation: "conv-1", removable: false } });
  });

  it("a message not delivered after Stop is stamped and removable only while the session offers it", () => {
    const [item] = buildTranscript([row("not_delivered")], summary) as ActivityItem[];
    expect(item).toMatchObject({ receipt: "Not delivered",
      inbox: { state: "not_delivered", messageId: "msg_queued01", run: "run-1", conversation: "conv-1", removable: true } });
    const [unoffered] = buildTranscript([row("not_delivered")], { ...summary, allowed_actions: ["rename", "forget"] }) as ActivityItem[];
    expect(unoffered.inbox?.removable).toBe(false);
    const [unknown] = buildTranscript([row("not_delivered")]) as ActivityItem[];
    expect(unknown.inbox?.removable).toBe(false);
    const [nameless] = buildTranscript([row("not_delivered", "")], summary) as ActivityItem[];
    expect(nameless.inbox?.removable).toBe(false);
  });

  it("an ordinary prompt keeps Sent and Queued at this time, and no stamp carries a separator dot", () => {
    const items = buildTranscript([{ seq: 1, type: "UserPromptSubmit", at: 10, detail: "sent" },
      { seq: 2, type: "UserPromptQueued", at: 11, detail: "waits" }, row("not_delivered")], summary) as ActivityItem[];
    expect(items.map((i) => i.receipt)).toEqual(["Sent", "Queued at this time", "Not delivered"]);
    expect(items.slice(0, 2).every((i) => i.inbox === undefined)).toBe(true);
    expect(items.every((i) => !i.receipt?.includes("\u00b7"))).toBe(true);
  });
});

describe("local message rows (ActivityView.cpp:665-720, 1053-1068)", () => {
  it("a native queued prompt keeps its queued label before answer labels", () => {
    const events: ActivityEvent[] = [{ seq: 1, type: "UserPromptQueued", at: 10, detail: "also lint",
      question_replies: [{ question: "Which checks?", answer: "Lint" }] }];
    expect(buildTranscript(events)[0]).toMatchObject({ label: "You (queued)", receipt: "Queued at this time" });
    events.push({ seq: 2, type: "UserPromptSubmit", at: 11, detail: "also lint" });
    expect(buildTranscript(events)[0]).toMatchObject({ label: "You (queued)", receipt: "Sent" });
  });

  it("local rows follow Qt's stamps and buttons", () => {
    const items = buildTranscript([], {}, [local("sending"), local("error"), local("error", true), local("sent")]) as ActivityItem[];
    expect(items.map((i) => i.local?.stamp)).toEqual(["Sending…", "Not sent", "Delivery not confirmed", "Submitted to terminal"]);
    expect(items[1].local?.actions).toEqual(["retry", "delete"]);
    expect(items[2].local?.actions).toEqual(["inspect", "delete"]);
    expect(items[2].local?.inspectLabel).toBe("Check Terminal");
    expect(buildTranscript([], { native_ui_available: true }, [local("error", true)])[0]).toMatchObject({ local: { inspectLabel: "Check native UI" } });
  });

  it("only a failed row has buttons and an error line; the stamp stands in for the time", () => {
    const items = buildTranscript([], {}, [local("sending"), local("sent"), local("error")]) as ActivityItem[];
    expect(items.map((i) => i.local?.actions)).toEqual([[], [], ["retry", "delete"]]);
    expect(items.map((i) => i.local?.error)).toEqual(["", "", "hgs: refused"]);
    expect(items.map((i) => [i.id, i.kind, i.at, i.body])).toEqual([
      ["local-l-sending-false", "user", "", "hello"], ["local-l-sent-false", "user", "", "hello"], ["local-l-error-false", "user", "", "hello"],
    ]);
    expect(items.every((i) => i.receipt === undefined)).toBe(true);
  });

  it("a row is placed by its time, after the events of the same time", () => {
    const events: ActivityEvent[] = [
      { seq: 1, type: "Stop", detail: "earlier reply", at: 100 },
      { seq: 2, type: "Stop", detail: "same time", at: 200 },
      { seq: 3, type: "Stop", detail: "later reply", at: 300 },
    ];
    const items = buildTranscript(events, {}, [local("error", false, { submitted_at: 200, text: "mine" })]) as ActivityItem[];
    expect(items.map((i) => i.body)).toEqual(["earlier reply", "same time", "mine", "later reply"]);
    const first = buildTranscript(events, {}, [local("error", false, { submitted_at: 50, text: "first" })]) as ActivityItem[];
    expect(first.map((i) => i.body)).toEqual(["first", "earlier reply", "same time", "later reply"]);
  });

  it("a sent row takes over its own prompt event instead of showing twice", () => {
    const events: ActivityEvent[] = [{ seq: 4, type: "UserPromptSubmit", detail: "hello there", at: 1_793_000_003 }];
    const sent = local("sent", false, { text: "hello there" });
    const items = buildTranscript(events, {}, [sent]) as ActivityItem[];
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ id: `local-${sent.id}`, kind: "user", body: "hello there", receipt: "Sent" });
    // More than 10 s away, or a failed row: never the same message.
    expect(buildTranscript(events, {}, [{ ...sent, submitted_at: 1_793_000_020 }])).toHaveLength(2);
    expect(buildTranscript(events, {}, [local("error", false, { text: "hello there" })])).toHaveLength(2);
    // A subagent's prompt is never this app's.
    expect(buildTranscript([{ ...events[0], agent_id: "a-1" }], {}, [sent]).filter((i) => i.kind === "user")).toHaveLength(1);
  });

  it("a sent row with files binds the one prompt with exactly its text, and its event shows the files", () => {
    const file = { name: "notes.txt", mime: "text/plain", bytes: 2048, reference: "[File #1]", request_id: "req-9", index: 0 };
    const sent = local("sent", false, { text: "see [File #1]", attachments: [file], submitted_text: "see [File #1]" });
    const events: ActivityEvent[] = [{ seq: 7, type: "UserPromptSubmit", detail: "see [File #1]", at: 1_793_000_500 }];
    const merged = mergeLocal(events, [sent]);
    expect(merged).toHaveLength(1);
    expect(merged[0]).toMatchObject({ seq: 7, activity_key: "local-l-sent-false", attachments: [file] });
    // Two such rows compete: neither binds, and both show as rows.
    expect(mergeLocal(events, [sent, { ...sent, id: "other" }])).toHaveLength(3);
    // Two exact events: Qt binds none without its remembered binding.
    expect(mergeLocal([...events, { ...events[0], seq: 8 }], [sent])).toHaveLength(3);
  });

  it("a voice row the journal already holds is left out", () => {
    const voice = local("sent", false, { id: "voice-m-1", source: "hgs_voice", message_id: "m-1", voice: { id: "m-1", state: "submitted", audio: {} } });
    expect(mergeLocal([{ seq: 1, source: "hgs_voice", message_id: "m-1", voice: { id: "m-1" }, at: 1 }], [voice])).toHaveLength(1);
    const items = buildTranscript([], {}, [voice]);
    expect(items).toMatchObject([{ kind: "media", id: "vm-1" }]);
  });

  it("a local row's file is available with its request or its cached copy (Qt's rule)", () => {
    expect(localFileAvailable({ request_id: "req-1" })).toBe(true);
    expect(localFileAvailable({ local_path: "/cache/ab.bin" })).toBe(true);
    expect(localFileAvailable({ request_id: "", local_path: "" })).toBe(false);
    expect(localFileAvailable({})).toBe(false);
    const row = local("error", false, { attachments: [{ name: "a.png", local_path: "/cache/a.bin" }, { name: "b.png" }] });
    expect((buildTranscript([], {}, [row])[0] as ActivityItem).local?.available).toEqual([true, false]);
  });

  it("without events the recorded context comes first, then the local rows", () => {
    const items = buildTranscript([], { prompt: "earlier request" }, [local("error")]) as ActivityItem[];
    expect(items.map((i) => i.kind)).toEqual(["context", "user", "user"]);
    expect(items[2].local?.stamp).toBe("Not sent");
    expect(buildTranscript([], { prompt: "earlier request" })).toHaveLength(2);
  });
});


describe("local-to-recorded display identity", () => {
  const event = (seq: number, detail = "hello", at = 1_793_000_001): ActivityEvent => ({seq,type:"UserPromptSubmit",detail,at});
  it("keeps one key and body before acknowledgement, at acknowledgement and after local receipt removal", () => {
    const projection=createTranscriptProjection(), row=local("sending",false,{id:"request-one",text:"**hello**\n\n- one\n- two"});
    const initial=projection.build([],{},[row]);
    const acknowledged=projection.build([event(4,row.text)],{},[{...row,status:"sent"}]);
    const recorded=projection.build([event(4,row.text)],{},[]);
    expect(initial).toHaveLength(1);expect(acknowledged).toHaveLength(1);expect(recorded).toHaveLength(1);
    expect([initial[0].id,acknowledged[0].id,recorded[0].id]).toEqual(Array(3).fill("local-request-one"));
    expect([(initial[0] as ActivityItem).body,(acknowledged[0] as ActivityItem).body,(recorded[0] as ActivityItem).body]).toEqual(Array(3).fill(row.text));
  });
  it("binds when Rust drops a sending receipt before the journal batch arrives, without a blank handoff frame", () => {
    const projection=createTranscriptProjection(),row=local("sending");projection.build([],{},[row]);
    expect(projection.build([],{},[]).map(item=>item.id)).toEqual([`local-${row.id}`]);
    expect(projection.build([event(8)],{},[]).map(item=>item.id)).toEqual([`local-${row.id}`]);
    projection.reset();expect(projection.build([event(8)],{},[]).map(item=>item.id)).toEqual(["s8"]);
  });
  describe("across a child visit (zerus-isrzi)", () => {
    const ids = (items: readonly { id: string }[]) => items.map((item) => item.id);
    const row = local("sent", false, { id: "sent-one" });
    /** A parent showing `row` after Rust dropped it, still before its journal event. */
    const inHandoff = () => {
      const build = createActivityProjections();
      build(false, "parent", [], {}, [row]);
      expect(ids(build(false, "parent", [], {}, []))).toEqual(["local-sent-one"]);
      return build;
    };
    it("keeps the parent's receipt through a visit, then binds it to its journal event", () => {
      const build = inHandoff();
      expect(build(true, "parent/child", [], {}, [])).toEqual([]);
      expect(ids(build(false, "parent", [], {}, []))).toEqual(["local-sent-one"]);
      expect(ids(build(false, "parent", [event(8)], {}, []))).toEqual(["local-sent-one"]);
    });
    it("keeps it through several children", () => {
      const build = inHandoff();
      build(true, "parent/one", [], {}, []); build(true, "parent/two", [], {}, []);
      expect(ids(build(false, "parent", [], {}, []))).toEqual(["local-sent-one"]);
    });
    it("shows a child's receipt in no other child and not in its parent", () => {
      const build = createActivityProjections();
      build(true, "parent/one", [], {}, [row]);
      expect(ids(build(true, "parent/one", [], {}, []))).toEqual(["local-sent-one"]);
      expect(build(true, "parent/two", [], {}, [])).toEqual([]);
      expect(build(true, "parent/one", [], {}, [])).toEqual([]);
      build(true, "parent/one", [], {}, [row]);
      expect(build(false, "parent", [], {}, [])).toEqual([]);
    });
    it("retains nothing of a session once another is shown", () => {
      const build = inHandoff();
      build(true, "parent/child", [], {}, [local("sent", false, { id: "child-one" })]);
      expect(build(false, "other", [], {}, [])).toEqual([]);
      expect(build(false, "parent", [], {}, [])).toEqual([]);
      expect(build(true, "parent/child", [], {}, [])).toEqual([]);
    });
    it("keeps its receipt when another session's child opens and that session is never shown", () => {
      const build = inHandoff();
      expect(build(true, "other/child", [], {}, [])).toEqual([]);
      expect(ids(build(false, "parent", [], {}, []))).toEqual(["local-sent-one"]);
    });
    it("stays within the projection's bound of 50 receipts", () => {
      const build = createActivityProjections();
      const many = Array.from({ length: 60 }, (_, i) => local("sent", false, { id: `sent-${i}`, text: `message ${i}` }));
      build(false, "parent", [], {}, many);
      build(true, "parent/child", [], {}, []);
      const kept = ids(build(false, "parent", [], {}, []));
      expect(kept).toHaveLength(50);
      expect(kept).toContain("local-sent-59");
      expect(kept).not.toContain("local-sent-9");
    });
  });
  it("never makes two distinct equal-text sends adopt one event or swap already bound keys", () => {
    const projection=createTranscriptProjection(),one=local("sent",false,{id:"one"}),two=local("sent",false,{id:"two",submitted_at:1_793_000_002});
    const first=projection.build([event(1)],{},[one]);expect(first.map(item=>item.id)).toEqual(["local-one"]);
    const both=projection.build([event(1),event(2,"hello",1_793_000_002)],{},[one,two]);
    expect(both.map(item=>item.id)).toEqual(["local-one","local-two"]);
    expect(projection.build([event(1),event(2,"hello",1_793_000_002)],{},[]).map(item=>item.id)).toEqual(["local-one","local-two"]);
    expect(mergeLocal([event(1)],[one,two])).toHaveLength(2);
  });
  it("preserves exact attachment receipts while keeping the local display key", () => {
    const projection=createTranscriptProjection();
    const file={name:"notes.txt",mime:"text/plain",bytes:12,reference:"[File #1]",request_id:"request",index:0};
    const row=local("sending",false,{id:"request",text:"Read [File #1]",attachments:[{...file,request_id:""}]});
    projection.build([],{},[row]);
    const items=projection.build([{...event(3,row.text),source:"hgs_delivery",message_id:"request",attachments:[file]}],{},[]) as ActivityItem[];
    expect(items).toHaveLength(1);expect(items[0].id).toBe("local-request");expect(items[0].attachments?.[0]).toMatchObject({request_id:"request",index:0});
  });
  it("does not resurrect a deleted failed local row or bind it to another prompt", () => {
    const projection=createTranscriptProjection(),row=local("error");projection.build([],{},[row]);
    expect(projection.build([event(1)],{},[]).map(item=>item.id)).toEqual(["s1"]);
  });
});

describe('real image receipt races',()=>{
 const shapes=[{width:2560,height:720},{width:720,height:2560},{width:80,height:50},{width:2400,height:1200},{width:800,height:1200}];
 for(const count of [1,2,3]) for(const shape of shapes) it(`retains geometry for ${count} image(s) at ${shape.width}x${shape.height} when journal precedes acknowledgement`,()=>{
  const files=Array.from({length:count},(_,index)=>({name:`image${index}.png`,mime:'image/png',bytes:100,reference:`[Image #${index+1}]`,image_size:shape,preview_path:`cache/${'a'.repeat(64)}/image${index}.png`}));
  const text='Above\n'+files.map(f=>f.reference).join('\n')+'\nBelow';
  const row=local('sending',false,{id:'request',text,attachments:files});
  const raw=text.replace(/\[Image #(\d+)\]/g,(_,n)=>`[Image #${n}] (Inspect the attached image at "/home/example/.local/state/hgs/sessions/attachments/message-test/${String(n).padStart(2,'0')}-image${Number(n)-1}.png")`);
  const event={seq:4,type:'UserPromptSubmit',at:row.submitted_at+.2,detail:raw};
  const metadata=files.map((f,index)=>{const {image_size,preview_path,...base}=f;void image_size;void preview_path;return {...base,request_id:'request',index};});
  const receipt={source:'hgs_delivery',type:'UserPromptSubmit',message_id:'request',detail:text,submitted_text:raw,attachments:metadata};
  const projection=createTranscriptProjection();const first=projection.build([],{},[row]) as ActivityItem[];
  const echo=projection.build([event],{},[row]) as ActivityItem[];
  // Rust may reconcile away the local row without publishing its Sent state.
  const recorded=projection.build([event],{attachment_messages:[receipt]},[]) as ActivityItem[];
  for(const items of [first,echo,recorded]){
   expect(items).toHaveLength(1);expect(items[0].id).toBe('local-request');expect(items[0].body).toBe(text);
   expect(items[0].attachments?.map(f=>[f.image_size,f.preview_path])).toEqual(files.map(f=>[f.image_size,f.preview_path]));
  }
  expect(recorded[0].attachments?.map(f=>[f.request_id,f.index])).toEqual(files.map((_,i)=>['request',i]));
 });
 it('cannot acquire a private local preview path from journal JSON',()=>{
  const items=buildTranscript([{seq:1,type:'UserPromptSubmit',attachments:[{name:'x.png',mime:'image/png',bytes:1,reference:'[Image #1]',request_id:'r',index:0,preview_path:`cache/${'a'.repeat(64)}/x.png`,image_size:{width:20,height:20}} as unknown as EventAttachment]}]) as ActivityItem[];
  expect(items[0].attachments?.[0].preview_path).toBeUndefined();expect(items[0].attachments?.[0].own).toBeUndefined();
 });
});

it('keeps equal-text image sends distinct when their separate receipts arrive',()=>{
 const image={name:'x.png',mime:'image/png',bytes:1,reference:'[Image #1]',image_size:{width:20,height:30},preview_path:`cache/${'a'.repeat(64)}/x.png`};
 const one=local('sending',false,{id:'one',text:'See [Image #1]',attachments:[image]}),two={...one,id:'two',submitted_at:one.submitted_at+1};
 const raw=(name:string)=>`See [Image #1] (Inspect the attached image at "/home/example/.local/state/hgs/sessions/attachments/message-${name}/01-x.png")`;
 const events=[{seq:1,type:'UserPromptSubmit',at:one.submitted_at+.2,detail:raw('one')},{seq:2,type:'UserPromptSubmit',at:two.submitted_at+.2,detail:raw('two')}];
 const projection=createTranscriptProjection();projection.build([],{},[one,two]);
 expect(projection.build(events,{},[one,two]).map(r=>r.id)).toEqual(['local-one','local-two']);
 const receipts=[one,two].map(r=>({source:'hgs_delivery',message_id:r.id,detail:r.text,submitted_text:raw(r.id),attachments:[{name:image.name,mime:image.mime,bytes:1,reference:image.reference,request_id:r.id,index:0}]}));
 const final=projection.build(events,{attachment_messages:receipts},[]) as ActivityItem[];
 expect(final.map(r=>r.id)).toEqual(['local-one','local-two']);expect(final.map(r=>r.attachments?.[0].request_id)).toEqual(['one','two']);
});

describe("main message window outside the tool-event tail", () => {
  const base = 1_793_000_000;
  const messages: ActivityEvent[] = Array.from({ length: 30 }, (_, i) => ({
    seq: 10 + i * 500, type: i % 2 ? "Stop" : "UserPromptSubmit", source: "codex_hooks",
    at: base + i * 300, detail: `${i % 2 ? "Reply" : "Request"} ${i}`,
  }));
  const tools: ActivityEvent[] = Array.from({ length: 100 }, (_, i) => ({
    seq: 25_000 + i, type: i % 2 ? "PostToolUse" : "PreToolUse", source: "codex_hooks",
    at: base + 40_000 + i, tool: "Bash", detail: `tool ${i}`,
  }));
  const summary = { message_events: messages, history_truncated: false, message_events_truncated: false };

  it("renders all 30 bounded journal messages before the 100 recent tools on cold open", () => {
    for (const build of [buildTranscript, createTranscriptProjection().build]) {
      const items = build(tools, summary);
      const conversation = items.filter(item => item.kind === "user" || item.kind === "agent");
      expect(conversation).toHaveLength(30);
      expect(conversation.map(item => "body" in item ? item.body : undefined)).toEqual(messages.map(event => event.detail));
      expect(items[0]).toMatchObject({ id: "s10", kind: "user", body: "Request 0" });
      expect(items.filter(item => item.kind === "tools")).toHaveLength(5);
    }
  });

  it("deduplicates by journal identity when an older tool page also carries the messages", () => {
    const projection = createTranscriptProjection();
    const before = projection.build(tools, summary);
    const after = projection.build([...messages, ...tools], summary);
    expect(after).toEqual(before);
    expect(new Set(after.map(item => item.id)).size).toBe(after.length);
    expect(tools).toHaveLength(100);
    expect(messages).toHaveLength(30);
  });

  it("merges a provider response with its older Stop and retains equal-text messages with different sequences", () => {
    const stop = { ...messages[1], agent_id: "main" };
    const window = [messages[0], stop, ...messages.slice(2), { ...messages[0], seq: 20, at: base + 1 }];
    const items = buildTranscript(tools, { ...summary, message_events: window,
      provider_messages: [{ type: "AgentMessage", source: "codex_transcript", message_id: "provider-1", agent_id: "main", at: stop.at, detail: `${stop.detail} in full` }] });
    expect(items.filter(item => item.kind === "user" && item.body === messages[0].detail)).toHaveLength(2);
    expect(items.filter(item => item.kind === "agent" && item.body === `${stop.detail} in full`)).toHaveLength(1);
    expect(items.filter(item => item.kind === "agent" && item.body === stop.detail)).toHaveLength(0);
  });
});

describe("a Stop that comes seconds after its transcript reply", () => {
  it("shows the reply once (the hook 6 s after the transcript)", () => {
    const reply = "DeepSeek 2.25 bits is only at the start: 20 tool calls in 18 minutes.";
    const events = [
      { seq: 1, type: "UserPromptSubmit", agent_id: "main", detail: "how is it going?", at: 100 },
      { seq: 2, type: "Stop", agent_id: "main", detail: reply, at: 106.6 },
    ];
    const items = buildTranscript(events, { provider_messages: [
      { type: "AgentMessage", source: "claude_transcript", message_id: "msg-1", agent_id: "main", at: 101, detail: reply },
    ] });
    expect(items.filter(item => item.kind === "agent" && item.body === reply)).toHaveLength(1);
  });

  it("keeps a later reply with the same opening that follows another agent message", () => {
    const events = [
      { seq: 1, type: "Stop", agent_id: "main", detail: "Done.", at: 100 },
      { seq: 2, type: "UserPromptSubmit", agent_id: "main", detail: "again", at: 110 },
      { seq: 3, type: "Stop", agent_id: "main", detail: "Done.", at: 130 },
    ];
    const items = buildTranscript(events, { provider_messages: [
      { type: "AgentMessage", source: "claude_transcript", message_id: "msg-1", agent_id: "main", at: 100, detail: "Done." },
    ] });
    expect(items.filter(item => item.kind === "agent" && item.body === "Done.")).toHaveLength(2);
  });
});

describe("bounded message-window input", () => {
  it("renders main UserMessage rows and excludes nested child messages and operational rows", () => {
    const items = buildTranscript([], { message_events: [
      { seq: 1, type: "UserMessage", agent_id: "main", detail: "Main request", at: 1 },
      { seq: 2, type: "Stop", agent_id: "child", detail: "Child reply", at: 2 },
      { seq: 3, type: "PreToolUse", detail: "Not a main message", at: 3 },
    ] });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: "user", body: "Main request" });
  });

  it("retains at most the native 100-message window without growing a history cache", () => {
    const message_events = Array.from({ length: 140 }, (_, i) => ({ seq: i + 1, type: "Stop", detail: `Reply ${i}`, at: i + 1 }));
    const items = buildTranscript([], { message_events });
    expect(items).toHaveLength(100);
    expect(items[0]).toMatchObject({ id: "s41", body: "Reply 40" });
    expect(items.at(-1)).toMatchObject({ id: "s140", body: "Reply 139" });
  });
});

// zerus-b4qk: a fork's Activity shows the history it began from, read-only.
describe("inherited fork history", () => {
  const ev = (seq: number, type: string, detail: string, inherited_from?: string): ActivityEvent =>
    ({ seq, type, at: seq, detail, agent_id: "", ...(inherited_from ? { inherited_from } : {}) });
  it("labels each source, marks its rows and shows where the fork begins", () => {
    const items = buildTranscript([
      ev(1, "UserPromptSubmit", "root request", "codex/p/root"),
      ev(2, "PreToolUse", "ls", "codex/p/root"),
      ev(3, "PostToolUse", "ls", "codex/p/root"),
      ev(4, "Stop", "root reply", "codex/p/root"),
      ev(5, "UserPromptSubmit", "first request", "codex/p/first"),
      ev(6, "PreToolUse", "cat", "codex/p/first"),
      ev(7, "PreToolUse", "rg"),
      ev(8, "UserPromptSubmit", "own request"),
      ev(9, "Stop", "own reply"),
    ]) as ActivityItem[];
    expect(items.map((i) => [i.kind, i.body, !!i.inherited])).toEqual([
      ["context", "Inherited from codex/p/root: the history this fork began from", false],
      ["user", "root request", true],
      ["tools", "ls", true],
      ["agent", "root reply", true],
      ["context", "Inherited from codex/p/first: the history this fork began from", false],
      ["user", "first request", true],
      // Tool groups never cross a source or the fork point.
      ["tool", "cat", true],
      ["context", "Forked here: this session's own activity continues below", false],
      ["tool", "rg", false],
      ["user", "own request", false],
      ["agent", "own reply", false],
    ]);
    // Never a local row's actions on an inherited row.
    expect(items.filter((i) => i.inherited).every((i) => !i.local)).toBe(true);
  });
  // #1235 review R2: an inherited recording is the source's, muted and never
  // played or opened through the fork.
  it("an inherited voice message is a muted card with no file in the fork", () => {
    const voice: ActivityEvent = { ...ev(1, "UserPromptSubmit", "spoken request", "codex/p/root"), source: "hgs_voice",
      voice: { state: "submitted", audio: { request_id: "source-request", index: 0, duration_ms: 1200 } } };
    const items = buildTranscript([voice, ev(2, "UserPromptSubmit", "own request")]);
    const card = items.find((i) => i.kind === "media") as MediaMessage;
    expect(card.inherited).toBe(true);
    expect(card.file).toMatchObject({ request_id: "source-request" });
    const scope = { computer: "studio", name: "codex/p/fork", conversation_id: "c-fork", archive_id: null };
    expect(cardTarget(card.file!, rowScope(card, scope))).toBeUndefined();
    expect(items.map((i) => [i.kind, !!(i as ActivityItem).inherited])).toEqual([["context", false], ["media", true], ["context", false], ["user", false]]);
  });
  it("a session that is not a fork has no inherited sections", () => {
    const items = buildTranscript([ev(1, "UserPromptSubmit", "request"), ev(2, "Stop", "reply")]) as ActivityItem[];
    expect(items.map((i) => i.kind)).toEqual(["user", "agent"]);
    expect(items.some((i) => i.inherited)).toBe(false);
  });
  it("a fork with only inherited rows so far ends on them, marked", () => {
    const items = buildTranscript([ev(1, "UserPromptSubmit", "request", "codex/p/root"), ev(2, "Stop", "reply", "codex/p/root")]) as ActivityItem[];
    expect(items.map((i) => [i.kind, !!i.inherited])).toEqual([["context", false], ["user", true], ["agent", true]]);
  });
});

describe("keep-warm ping turns", () => {
  const PING = "[Zerus keep-warm] Reply with exactly OK and do nothing else.";
  const row = (seq: number, type: string, detail: string, extra: Partial<ActivityEvent> = {}): ActivityEvent => ({ seq, type, at: 1_793_000_000 + seq, detail, agent_id: "", ...extra });

  it("folds the ping, its reply and its Stop into one dim row", () => {
    const items = buildTranscript([
      row(1, "UserPromptSubmit", "Fix the tests"), row(2, "Stop", "Done."),
      row(3, "UserPromptSubmit", PING, { origin: "keep_warm", turn_origin: "keep_warm" }),
      // The provider's own copy of the reply carries no mark.
      { seq: 0, type: "AgentMessage", at: 1_793_000_003.5, detail: "OK", source: "provider" },
      row(4, "Stop", "OK", { turn_origin: "keep_warm" }),
      row(5, "UserPromptSubmit", "Next task"), row(6, "Stop", "Done again."),
    ]) as ActivityItem[];
    expect(items.map((i) => [i.kind, i.title ?? i.body])).toEqual([
      ["user", "Fix the tests"], ["agent", "Done."], ["warm", "Cache kept warm"], ["user", "Next task"], ["agent", "Done again."],
    ]);
    expect(items[2].lines).toEqual([PING, "OK"]);
  });

  it("a running ping and a stray tagged row stay out of the person's messages", () => {
    const running = buildTranscript([row(1, "Stop", "Done."), row(2, "UserPromptSubmit", PING, { origin: "keep_warm", turn_origin: "keep_warm" })]) as ActivityItem[];
    expect(running.map((i) => i.kind)).toEqual(["agent", "warm"]);
    const stray = buildTranscript([row(1, "Stop", "OK", { turn_origin: "keep_warm" }), row(2, "UserPromptSubmit", "Hello")]) as ActivityItem[];
    expect(stray.map((i) => i.kind)).toEqual(["user"]);
  });
});

describe("tool rows (ActivityView.cpp:1105-1140, zerus-5wmh.13.8.1)", () => {
  // The preview as it was: the whole text flattened, then cut.
  const before = (text: string) => {
    const flat = text.replace(/\s+/g, " ").trim();
    return flat.length > 110 ? `${flat.slice(0, 109)}…` : flat;
  };
  const big = "line of output\n".repeat(400_000);

  it("an expanded row carries each event's whole output, and the collapsed row keeps its preview", () => {
    const process = "a".repeat(32);
    const items = buildTranscript([
      { seq: 1, type: "PreToolUse", tool: "Bash", at: 10, detail: "npm test", process_id: process },
      { seq: 2, type: "PostToolUse", tool: "Bash", at: 11, detail: big },
    ]) as ActivityItem[];
    expect(items).toHaveLength(1);
    const [row] = items;
    expect(row).toMatchObject({ kind: "tools", body: "Bash" });
    expect(row.events!.map((e) => [e.label, e.size, e.process])).toEqual([
      ["Tool started / Bash", 8, process], ["Tool finished / Bash", big.length, undefined],
    ]);
    expect(row.events![1].detail()).toBe(big);
    // What Activity serializes to see a row change stays small.
    expect(JSON.stringify(row).length).toBeLessThan(1000);
  });

  it("a single event keeps its 110-character preview", () => {
    const [row] = buildTranscript([{ seq: 1, type: "PostToolUse", tool: "Read", at: 10, detail: big }]) as ActivityItem[];
    expect(row).toMatchObject({ kind: "tool", title: "Tool finished / Read", body: before(big) });
    expect(row.events![0].detail()).toBe(big);
  });

  it("the preview reads only as far as it shows, with the same text as before", () => {
    const texts = [
      "", "   ", "short", "  lead and trail  ", "a\n\tb\r\nc", "x".repeat(109), "x".repeat(110), "x".repeat(111),
      `${" ".repeat(5000)}${"word ".repeat(100)}`, `${"y".repeat(108)}  \n  z`, `${"y".repeat(109)} z`,
      "\u00a0nbsp\u2028line\u2029para\ufeffbom\u3000ideo", `${"😀".repeat(60)}`, big,
    ];
    for (const text of texts) expect(toolPreview(text)).toBe(before(text));
    expect(toolPreview(undefined)).toBe("");
  });

  it("reads at most a bounded start of a whitespace-heavy output, and says when it stopped there (zerus-1nsc6)", () => {
    const spaces = " ".repeat(5_000_000);
    // Whitespace before, as, or after the preview: the preview reads only TOOL_PREVIEW_SCAN characters.
    expect(toolPreview(`${spaces}abcd`)).toBe("…");
    expect(toolPreview(spaces)).toBe("…");
    expect(toolPreview(`${"x".repeat(110)}${spaces}`)).toBe(`${"x".repeat(109)}…`);
    expect(toolPreview(`${"x".repeat(50)}${spaces}tail`)).toBe(`${"x".repeat(50)}…`);
    // Within the bound, the preview is the one it always was.
    expect(toolPreview(`${" ".repeat(TOOL_PREVIEW_SCAN - 10)}abcd`)).toBe("abcd");
    expect(toolPreview(" ".repeat(TOOL_PREVIEW_SCAN))).toBe("");
    const started = performance.now();
    for (let i = 0; i < 100; i++) toolPreview(`${spaces}${i}`);
    expect(performance.now() - started).toBeLessThan(2000);
  });
});
