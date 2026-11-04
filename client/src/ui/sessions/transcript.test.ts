import { describe, expect, it } from "vitest";
import { buildTranscript, localFileAvailable, mergeLocal, createTranscriptProjection } from "./transcript";
import type { ActivityEvent, EventAttachment, LocalMessage } from "../../types";
import type { ActivityItem } from "./types";

const local = (status: "sending" | "error" | "sent", uncertain = false, extra: Partial<LocalMessage> = {}): LocalMessage => ({
  id: `l-${status}-${uncertain}`, text: "hello", status, attachments: [], run_id: "r", conversation_id: "c",
  submitted_at: 1_793_000_000, error: status === "error" ? "hgs: refused" : "", uncertain, message_id: null, submitted_text: null, source: null, voice: null,
  ...extra,
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
