import type { ActivityEvent, LocalMessage, Reply } from "../../types";
import { eventKey } from "../../store/activity";
import type { ActivityItem, LocalItem, MediaMessage, TranscriptItem, VoiceState } from "./types";
import { imageSize } from "./attachment-image";
import { messageTimeOf } from "../lib/dates";
import { tr, trNoop } from "../../i18n";

// A Symbol cannot arrive from a provider's JSON. Only our own local send
// supplies these immutable-cache preview paths and oriented dimensions.
const LOCAL_FILES = Symbol("local attachment presentation");
type DisplayEvent = ActivityEvent & { [LOCAL_FILES]?: LocalMessage["attachments"] };

// Journal events as the Activity items of the approved design, after the
// rules of tray/src/ActivityView.cpp: messages by role, thinking, cleared
// sessions, notices (subagent reports, peer messages, background tasks),
// voice and video messages, and adjacent operational events collapsed into one
// tool group (at most 24, never across a message or an attention event).

/** Delivery labels, as in ActivityView.cpp (unknown states read "Delivery not confirmed"). */
export const VOICE_STATE_LABEL: Record<VoiceState, string> = {
  uploading: trNoop("Sending recording…"),
  queued: trNoop("Waiting for recognition"),
  transcribing: trNoop("Recognizing…"),
  ready: trNoop("Ready to deliver"),
  waiting_agent: trNoop("Waiting for agent"),
  delivering: trNoop("Delivering…"),
  submitted: trNoop("Accepted by native app"),
  failed: trNoop("Voice message failed"),
  cancelled: trNoop("Cancelled"),
  uncertain: trNoop("Delivery not confirmed"),
};

/** Tooltip detail: only Accepted explains what it does not confirm. */
export function voiceStateDetail(state: VoiceState): string {
  return state === "submitted"
    ? tr("The native app accepted the recognized text. Queue status, when present, is shown alongside this recording. This does not confirm model delivery, reading or completion.")
    : tr(VOICE_STATE_LABEL[state]);
}

/** States drawn with the warning glyph (VoicePlayerImage::deliveryIcon). */
export const voiceStateIssue = (state: VoiceState) => state === "failed" || state === "uncertain";

const EVENT_TITLE: Record<string, string> = {
  SessionStart: trNoop("Session started"), SessionEnd: trNoop("Session closed"),
  TurnStarted: trNoop("Turn started"), UserPromptQueued: trNoop("Message queued"),
  PreToolUse: trNoop("Tool started"), PostToolUse: trNoop("Tool finished"),
  PostToolUseFailure: trNoop("Tool failed"), PermissionRequest: trNoop("Approval requested"),
  PermissionResult: trNoop("Approval resolved"), Stop: trNoop("Response finished"),
  StopFailure: trNoop("Response failed"), Interrupt: trNoop("Turn interrupted"),
  PreCompact: trNoop("Compacting context"), PostCompact: trNoop("Context compacted"),
  SubagentStart: trNoop("Subagent started"), SubagentStop: trNoop("Subagent finished"),
  TaskStarted: trNoop("Background task started"), Notification: trNoop("Agent notification"),
};
export const eventTitle = (type: string | undefined) => (type ? (type in EVENT_TITLE ? tr(EVENT_TITLE[type]) : type) : tr("Activity"));

/** "Oct 27 09:12" ("27 октября 09:12" in Russian), local time, as the design shows message times. */
export function timeText(at: number | undefined): string {
  if (!at || at <= 0) return "";
  return messageTimeOf(new Date(at * 1000));
}

type Role = "user" | "assistant" | "notice" | "";
const childEvent = (event: ActivityEvent): event is ActivityEvent & { agent_id: string } => !!event.agent_id && event.agent_id !== "main";

/** ActivityView.cpp messageRole: who wrote an event, if anyone. */
export function messageRole(e: ActivityEvent): Role {
  if (e.source === "zerus_task" && e.type === "TaskEntry") return e.task_role === "person" ? "user" : e.task_role === "agent" ? "assistant" : "";
  if (e.source === "hgs_voice" && e.voice && typeof e.voice === "object") return "user";
  if (childEvent(e) || (!e.detail && !(e.attachments?.length))) return "";
  if (e.origin === "task_notification" || e.origin === "subagent_report" || e.origin === "peer_message") return "notice";
  switch (e.type) {
    case "UserPromptSubmit": case "UserPromptQueued": case "UserMessage": case "TurnStarted": case "QuestionAnswered": case "LocalMessage": return "user";
    case "Stop": case "AgentMessage": return "assistant";
  }
  return "";
}

/** Approvals and failures open their group and are never merged with others. */
const attention = (e: ActivityEvent) => e.type === "PermissionRequest" || e.type === "PostToolUseFailure" || e.type === "StopFailure" || e.type === "Interrupt";


/** A message's files as Qt's attachment cards show them (ActivityView.cpp:759-782). */
export function attachments(e: ActivityEvent): ActivityItem["attachments"] {
  const list = Array.isArray(e.attachments) ? e.attachments : [];
  if (!list.length) return undefined;
  return list.map((a, index) => {
    const text = (value: unknown) => (typeof value === "string" ? value : "");
    const name = text(a?.name), reference = text(a?.reference), mime = text(a?.mime);
    const bytes = typeof a?.bytes === "number" && Number.isFinite(a.bytes) ? a.bytes : 0;
    const local = (e as DisplayEvent)[LOCAL_FILES]?.[index];
    const own = !!local && local.name === name && local.mime === mime && local.bytes === bytes && local.reference === reference;
    const preview = own && typeof local.preview_path === "string" && /^cache\/[a-f0-9]{64}\/[^/\\]+$/u.test(local.preview_path) ? local.preview_path : undefined;
    return {
      name,
      caption: (reference ? `${reference} ` : "") + name,
      mime,
      meta: bytes > 0 ? `${Math.max(1, Math.ceil(bytes / 1024))} KiB` : "File",
      request_id: text(a?.request_id),
      index: typeof a?.index === "number" && Number.isInteger(a.index) ? a.index : -1,
      image: mime.startsWith("image/") && mime !== "image/svg+xml",
      ...(own ? { own: true, preview_path: preview, image_size: imageSize(local.image_size) } : {}),
    };
  });
}

interface AttachmentEcho {
  reference: string;
  kind: "Image" | "File";
  path: string;
}

/** The numbered terminal echoes produced by attachments::terminal_text. */
export function attachmentEchoes(text: string): AttachmentEcho[] {
  const pattern = /\[(Image|File) #([1-9][0-9]{0,8})\] ?\((Inspect the attached image at|Read the attached file at) ("(?:[^"\\]|\\.)*") ?\)/g;
  const echoes: AttachmentEcho[] = [];
  for (const found of text.matchAll(pattern)) {
    const kind = found[1] as AttachmentEcho["kind"];
    if ((kind === "Image") !== (found[3] === "Inspect the attached image at")) continue;
    try {
      const path = JSON.parse(found[4]);
      if (typeof path === "string") echoes.push({ reference: found[0].slice(0, found[0].indexOf("]") + 1), kind, path });
    } catch { /* A malformed path stays ordinary message text. */ }
  }
  return echoes;
}

function echoMatchesFiles(text: string, files: ActivityEvent["attachments"]): boolean {
  const echoes = attachmentEchoes(text);
  if (!files?.length || echoes.length !== files.length) return false;
  return echoes.every((echo, index) => {
    const file = files[index];
    const name = typeof file?.name === "string" ? file.name : "";
    const reference = typeof file?.reference === "string" ? file.reference : "";
    const mime = typeof file?.mime === "string" ? file.mime : "";
    const filename = `${String(index + 1).padStart(2, "0")}-${name.replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 100)}`;
    const path = echo.path.replaceAll("\\", "/");
    return echo.reference === reference && (echo.kind === "Image") === mime.startsWith("image/")
      && /(?:^|\/)attachments\/message-[A-Za-z0-9]+\/[^/]+$/.test(path) && path.split("/").at(-1) === filename;
  });
}

/**
 * Reconciles a native numbered prompt echo with its one exact delivery receipt.
 * Public receipt metadata supplies the request and index; an echoed host path
 * is never used to read a file. Unknown or ambiguous echoes stay untouched.
 */
export function mergeAttachmentMessages(given: readonly ActivityEvent[], summary?: Record<string, unknown>): readonly ActivityEvent[] {
  const receipts = Array.isArray(summary?.attachment_messages) ? summary.attachment_messages as ActivityEvent[] : [];
  if (!receipts.length) return given;
  const events = [...given];
  let changed = false;
  for (const receipt of receipts) {
    const files = Array.isArray(receipt.attachments) ? receipt.attachments : [];
    const submitted = typeof receipt.submitted_text === "string" ? receipt.submitted_text.trim() : "";
    if (receipt.source !== "hgs_delivery" || !files.length || !submitted || !echoMatchesFiles(submitted, files)) continue;
    if (receipts.filter((other) => typeof other.submitted_text === "string" && other.submitted_text.trim() === submitted).length !== 1) continue;
    const exact: number[] = [];
    events.forEach((event, index) => {
      if (event.source !== "hgs_voice" && !childEvent(event) && !event.attachments?.length && PROMPTS.has(event.type ?? "")
        && (event.detail ?? "").trim() === submitted) exact.push(index);
    });
    if (exact.length !== 1) continue;
    const at = exact[0];
    const event = events[at];
    events[at] = {
      ...event, detail: typeof receipt.detail === "string" ? receipt.detail : "", attachments: files,
      attachment_echo: event.detail, attachment_request_id: receipt.message_id, attachment_submitted_text: receipt.submitted_text,
    };
    changed = true;
  }
  return changed ? events : given;
}

function media(e: ActivityEvent, id: string): MediaMessage {
  const voice = (e.voice ?? {}) as Record<string, unknown>;
  const audio = (voice.audio ?? {}) as Record<string, unknown>;
  const video = (voice.video ?? undefined) as Record<string, unknown> | undefined;
  const state = String(voice.state ?? "uncertain") as VoiceState;
  const waveform = Array.isArray(audio.waveform) ? (audio.waveform as unknown[]).map((v) => Math.max(0, Math.min(255, Number(v) || 0))) : [];
  const file = video && Object.keys(video).length ? video : audio;
  const index = Number(file.index);
  const message = typeof e.message_id === "string" && e.message_id || (typeof voice.id === "string" ? voice.id : "");
  return {
    id, kind: "media", media: video && Object.keys(video).length ? "video" : "voice", at: timeText(e.at),
    state: state in VOICE_STATE_LABEL ? state : "uncertain",
    durationMs: Math.max(0, Number(audio.duration_ms) || 0),
    waveform: waveform.length === 64 ? waveform : [],
    posterMessageId: video?.poster_available === true && message ? message : undefined,
    file: typeof audio.request_id === "string" && !!audio.request_id && typeof file.request_id === "string" && !!file.request_id
      && typeof file.index === "number" && Number.isSafeInteger(index) && index >= 0 ? { request_id: file.request_id, index } : undefined,
    transcript: typeof voice.transcript === "string" && voice.transcript ? voice.transcript : undefined,
    error: typeof voice.error === "string" && voice.error ? voice.error : undefined,
  };
}

const GROUP_LIMIT = 24;
const PREVIEW = 110;
const preview = (text: string | undefined) => {
  const flat = (text ?? "").replace(/\s+/g, " ").trim();
  return flat.length > PREVIEW ? `${flat.slice(0, PREVIEW - 1)}…` : flat;
};

/**
 * The request and reply the summary recorded, when the timeline has no events
 * (a journal that kept nothing of this conversation). As in Qt, they are shown
 * as preceding context, never as new messages.
 */
function recordedContext(summary: Record<string, unknown> | undefined): TranscriptItem[] {
  const prompt = typeof summary?.prompt === "string" ? summary.prompt.trim() : "";
  const answer = typeof summary?.last_message === "string" ? summary.last_message.trim() : "";
  if (!prompt && !answer) return [];
  const items: TranscriptItem[] = [{ id: "context", kind: "context", at: "", body: tr("Recorded context — outside the available timeline") }];
  // A recorded prompt that holds Codex replies reads as one (Rust's prompt_replies).
  const replies = Array.isArray(summary?.prompt_replies) && summary.prompt_replies.length ? summary.prompt_replies as Reply[] : undefined;
  if (prompt) items.push({ id: "prompt-snapshot", kind: "user", at: "", label: replies ? tr("You (recorded answer)") : tr("You (recorded request)"), body: prompt, replies });
  if (answer) items.push({ id: "answer-snapshot", kind: "agent", at: "", label: tr("Agent (recorded response)"), body: answer });
  return items;
}

// ---------------------------------------------------------------------------
// Local message rows (Qt's m_localMessages): what this app sent until the
// agent's own history shows it, and what failed (ActivityView.cpp:665-720,
// 1053-1068).

const PROMPTS = new Set(["UserPromptSubmit", "UserPromptQueued", "TurnStarted"]);
const LOCAL_STAMP: Record<LocalMessage["status"], string> = { sending: trNoop("Sending…"), error: trNoop("Not sent"), sent: trNoop("Submitted to terminal") };
/** How far a sent row's own prompt event may be from it, in seconds (ActivityView.cpp:684). */
const NEAREST = 10;

/** The text the agent received (the receipt's, else the row's), trimmed. */
const submittedOf = (row: LocalMessage) => (row.submitted_text ?? row.text).trim();

/**
 * Whether a local row's file can be opened: it has its receipt's request or
 * its cached copy (Qt's rule, ActivityView.cpp:765). Activity's journal cards
 * read a file by its request only (attachment-card.ts `cardTarget`).
 */
export const localFileAvailable = (file: Record<string, unknown>) =>
  (typeof file.request_id === "string" && file.request_id !== "") || (typeof file.local_path === "string" && file.local_path !== "");

/** A failed or pending row's stamp, error line and buttons (ActivityView.cpp:1053-1068). */
export function localItem(row: LocalMessage, summary?: Record<string, unknown>): LocalItem {
  const failed = row.status === "error";
  return {
    id: row.id,
    stamp: failed && row.uncertain ? tr("Delivery not confirmed") : tr(LOCAL_STAMP[row.status] ?? LOCAL_STAMP.sent),
    error: failed ? row.error : "",
    uncertain: failed && row.uncertain,
    // A delivery that may have happened is never retried from here: Check, then Allow retry in the composer.
    actions: failed && row.id ? (row.uncertain ? ["inspect", "delete"] : ["retry", "delete"]) : [],
    inspectLabel: summary?.native_ui_available === true ? tr("Check native UI") : tr("Check Terminal"),
    available: row.attachments.map(localFileAvailable),
  };
}

/** A local row as a timeline event (Qt's `type: LocalMessage`). */
function localEvent(row: LocalMessage): DisplayEvent {
  return {
    type: "LocalMessage", detail: row.text, at: row.submitted_at, attachments: row.attachments as ActivityEvent["attachments"],
    source: row.source ?? undefined, voice: row.voice ?? undefined, message_id: row.message_id ?? undefined, local: row,
    [LOCAL_FILES]: row.attachments,
  };
}

/**
 * The events with the session's local rows merged in (ActivityView.cpp:665-720).
 * A voice row the journal already holds is left out. A sent row takes over its
 * own prompt event, which then shows its text and files: the one event with
 * exactly its text when it has files and no other sent row competes, else the
 * nearest prompt within 10 s that starts like it. Every other row is placed by
 * its time, after the events of the same time.
 */
export function mergeLocal(events: readonly ActivityEvent[], local: readonly LocalMessage[], bindingOnly: readonly LocalMessage[] = []): ActivityEvent[] {
  const source = events.slice();
  const appended: ActivityEvent[] = [];
  const claimed = new Set<number>();
  const candidates = [...local, ...bindingOnly];
  for (const row of candidates) {
    const append = local.includes(row);
    if (row.source === "hgs_voice") {
      if (append && !source.some((e) => e.source === "hgs_voice" && e.message_id === row.message_id)) appended.push(localEvent(row));
      continue;
    }
    const submitted = submittedOf(row);
    const sent = row.status !== "error";
    let match = -1;
    const exact: number[] = [];
    const eligible = (e: ActivityEvent, i: number) => !claimed.has(i) && !childEvent(e) && e.source !== "hgs_voice" && PROMPTS.has(e.type ?? "")
      && (!e.activity_display_key || e.activity_display_key === `local-${row.id}`);
    if (sent) match = source.findIndex((e, i) => eligible(e, i) && e.activity_display_key === `local-${row.id}`);
    if (sent && match < 0) match = source.findIndex((e, i) => eligible(e, i)
      && !!(e.message_id || e.attachment_request_id) && [row.id, row.message_id].some(id => !!id && (e.message_id === id || e.attachment_request_id === id)));
    if (sent && match < 0 && row.attachments.length) {
      source.forEach((e, i) => {
        if (eligible(e, i) && !e.attachments?.length
          && submitted && (e.detail ?? "").trim() === submitted) exact.push(i);
      });
      const competing = candidates.filter((other) => other.status !== "error" && other.attachments.length && submittedOf(other) === submitted).length;
      // Qt also remembers which of several exact events a row was bound to; with one it is that one.
      if (competing === 1 && exact.length === 1) match = exact[0];
    }
    if (sent && match < 0 && !exact.length) {
      let nearest = NEAREST;
      const head = submitted.slice(0, 1000);
      source.forEach((e, i) => {
        if (!eligible(e, i)) return;
        const body = (e.detail ?? "").trim();
        const distance = Math.abs((e.at ?? NaN) - row.submitted_at);
        if (e.source !== "hgs_voice" && !e.attachments?.length && distance <= nearest && submitted && body
          && (body.startsWith(head) || submitted.startsWith(body.slice(0, 1000)))) { match = i; nearest = distance; }
      });
    }
    if (match >= 0) {
      claimed.add(match);
      source[match] = {
        ...source[match], activity_journal_key: eventKey(source[match]), activity_key: `local-${row.id}`, activity_display_key: `local-${row.id}`, detail: row.text,
        attachments: source[match].attachments?.length ? source[match].attachments : row.attachments as ActivityEvent["attachments"], attachment_submitted_text: row.submitted_text,
        [LOCAL_FILES]: row.attachments,
      } as DisplayEvent;
      continue;
    }
    if (append) appended.push(localEvent(row));
  }
  // A native echo may precede both the CLI acknowledgement and its durable
  // receipt. Keep the client's row visible through that short gap; do not
  // invent an identity binding from text. Equal-text sends remain separate.
  const visible = source.filter((event, index) => claimed.has(index) || !candidates.some((row) => {
    if (row.status === "error" || !row.attachments.length || !PROMPTS.has(event.type ?? "") || childEvent(event)
      || event.attachments?.length || Math.abs((event.at ?? NaN) - row.submitted_at) > NEAREST) return false;
    const raw = event.detail ?? "";
    if (!echoMatchesFiles(raw, row.attachments as ActivityEvent["attachments"])) return false;
    const canonical = raw.replace(/\[(Image|File) #([1-9][0-9]{0,8})\] ?\((?:Inspect the attached image at|Read the attached file at) (?:"(?:[^"\\]|\\.)*") ?\)/g, "[$1 #$2]");
    return canonical.trim() === row.text.trim();
  }));
  if (!appended.length) return visible;
  // Placed by time, after the events of the same time (Qt's stable sort by `at`).
  const placed = appended.map((e, i) => ({ e, i })).sort((a, b) => (a.e.at ?? 0) - (b.e.at ?? 0) || a.i - b.i).map((p) => p.e);
  const merged: ActivityEvent[] = [];
  let next = 0;
  for (const e of visible) {
    const at = typeof e.at === "number" ? e.at : 0;
    while (next < placed.length && at > 0 && (placed[next].at ?? 0) < at) merged.push(placed[next++]);
    merged.push(e);
  }
  while (next < placed.length) merged.push(placed[next++]);
  return merged;
}

const PROVIDER_SOURCES = new Set(["codex_transcript", "claude_transcript", "kimi_wire"]);
const MESSAGE_TYPES = new Set(["SessionCleared", "UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted", "QuestionAnswered", "AgentMessage", "Stop"]);

/** The inspection keeps up to 100 main journal messages independently of its
 * tool-event tail. Merge that bounded window for display only; tool cursors and
 * older-page requests continue to use the source's original journal events. */
export function mergeMessages(given: readonly ActivityEvent[], summary?: Record<string, unknown>): readonly ActivityEvent[] {
  if (!Array.isArray(summary?.message_events) || !summary.message_events.length) return given;
  const events = [...given];
  const known = new Set(given.map(eventKey));
  for (const value of summary.message_events.slice(-100)) {
    if (!value || typeof value !== "object") continue;
    const message = value as ActivityEvent;
    if (!MESSAGE_TYPES.has(message.type ?? "") || childEvent(message)
      || typeof message.seq !== "number" || !Number.isSafeInteger(message.seq) || message.seq <= 0) continue;
    const key = eventKey(message);
    if (known.has(key)) continue;
    known.add(key);
    events.push(message);
  }
  if (events.length === given.length) return given;
  if (events.every(event => typeof event.seq === "number" && event.seq > 0)) events.sort((a, b) => a.seq! - b.seq!);
  else if (events.every(event => typeof event.at === "number" && Number.isFinite(event.at) && event.at > 0)) events.sort((a, b) => a.at! - b.at!);
  return events;
}
const isExcerpt = (excerpt: string, full: string) => {
  const prefix = excerpt.endsWith("…") ? excerpt.slice(0, -1) : excerpt;
  return !!prefix && full.startsWith(prefix);
};

/**
 * Qt's provider_messages merge: watch event frames also arrive here, folded
 * into the summary by the core. Keep journal cursors intact and project the
 * bounded provider window at render time, including progress between tools.
 */
export function mergeProvider(given: readonly ActivityEvent[], summary?: Record<string, unknown>): readonly ActivityEvent[] {
  if (!Array.isArray(summary?.provider_messages) || !summary.provider_messages.length) return given;
  const events = [...given];
  const matchedStops = new Set<number>();
  const identities = new Set<string>();
  const seconds = new Map<number, number[]>();
  const nativeKey = (e: ActivityEvent) => e.message_id ? `${e.source}:${e.type}:${e.message_id}` : "";
  const at = (e: ActivityEvent) => {
    const value = e.at;
    return typeof value === "number" && Number.isFinite(value) ? value : 0;
  };
  // Index only matching candidates once, rather than scanning every loaded
  // journal page for each provider row on every streaming rebuild.
  const index = (e: ActivityEvent, i: number) => {
    if (childEvent(e) || (e.type !== "Stop" && !PROVIDER_SOURCES.has(e.source ?? ""))) return;
    const key = nativeKey(e);
    if (key) identities.add(key);
    const second = Math.floor(at(e));
    const bucket = seconds.get(second);
    if (bucket) bucket.push(i); else seconds.set(second, [i]);
  };
  events.forEach(index);
  let inserted = false;
  for (const value of summary.provider_messages.slice(-100)) {
    if (!value || typeof value !== "object") continue;
    const message = value as ActivityEvent;
    if (!PROVIDER_SOURCES.has(message.source ?? "") || childEvent(message)
      || (message.type !== "AgentMessage" && !(message.type === "AgentThinking" && message.source === "claude_transcript"))) continue;
    const key = nativeKey(message);
    if (key && identities.has(key)) continue;
    const when = at(message);
    const nearby: number[] = [];
    for (let offset = -3; offset <= 3; offset++) {
      for (const i of seconds.get(Math.floor(when) + offset) ?? []) if (Math.abs(at(events[i]) - when) <= 3) nearby.push(i);
    }
    nearby.sort((a, b) => a - b);
    const same = nearby.some((i) => {
      const e = events[i];
      return e.type === message.type && e.source === message.source
        && !(message.message_id && e.message_id && message.message_id !== e.message_id)
        && e.detail === message.detail && Math.abs(at(e) - when) < 3;
    });
    if (same) continue;
    // The hook's final excerpt and the provider's full response are one row.
    // An empty Stop never absorbs commentary immediately before completion.
    const stop = message.type === "AgentMessage" ? nearby.find((i) => !matchedStops.has(i) && events[i].type === "Stop"
      && Math.abs(at(events[i]) - when) <= 2
      && (events[i].detail ? isExcerpt(events[i].detail!, message.detail ?? "") : message.message_phase !== "commentary")) : undefined;
    if (stop !== undefined) {
      matchedStops.add(stop);
      events[stop] = { ...events[stop], detail: message.detail, source: message.source, message_id: message.message_id };
      if (key) identities.add(key);
    } else {
      index(message, events.length);
      events.push(message);
      inserted = true;
    }
  }
  // Preserve journal order when no row was inserted, or occurrence times are
  // incomplete. Fully timed rows use occurrence time with sequence tie breaks.
  if (inserted && events.every((e) => at(e) > 0)) {
    events.sort((a, b) => at(a) - at(b) || ((a.seq ?? 0) > 0 && (b.seq ?? 0) > 0 ? a.seq! - b.seq! : 0));
  }
  return events;
}

/**
 * The items of a transcript, oldest first; ids are stable while events only
 * append. `local` are the open session's local rows (the composer view's).
 */
export function buildTranscript(given: readonly ActivityEvent[], summary?: Record<string, unknown>, local: readonly LocalMessage[] = []): TranscriptItem[] {
  const provider = mergeAttachmentMessages(mergeProvider(mergeMessages(given, summary), summary), summary);
  if (!provider.length && !local.length) return recordedContext(summary);
  const events = mergeAttachmentMessages(local.length ? mergeLocal(provider, local) : provider, summary);
  return transcriptItems(events, summary, !provider.length);
}

/** Journal identity stays intact in the store; only rendered rows adopt a send's key. */
function transcriptItems(events: readonly ActivityEvent[], summary?: Record<string, unknown>, context = false): TranscriptItem[] {
  const items: TranscriptItem[] = context ? recordedContext(summary) : [];
  for (let i = 0; i < events.length;) {
    const e = events[i];
    const id = typeof e.activity_display_key === "string" ? e.activity_display_key : eventKey(e);
    const role = messageRole(e);
    const at = timeText(e.at);
    if (e.type === "SessionCleared") {
      items.push({ id, kind: "cleared", at, sender: tr("Session cleared"), body: tr("The agent no longer sees the activity above. New messages start with a fresh conversation context.") });
      i++; continue;
    }
    if (e.type === "AgentThinking") {
      if ((e.detail ?? "").trim()) items.push({ id, kind: "thinking", at, body: e.detail });
      i++; continue;
    }
    if (role === "notice") {
      const report = typeof e.report === "string" ? e.report : "";
      if (e.origin === "subagent_report") items.push({ id, kind: "report", at, title: preview(e.detail) || tr("Subagent report"), body: report || e.detail, sender: e.sender || undefined });
      else items.push({ id, kind: "system", at, sender: e.origin === "peer_message" ? tr("Message from %1", e.sender ?? tr("another session")) : tr("Background task"), body: report || e.detail });
      i++; continue;
    }
    if (role === "user" && e.source === "hgs_voice") {
      items.push(media(e, id));
      i++; continue;
    }
    if (e.type === "LocalMessage" && role === "user") {
      // The stamp stands in for the time and the receipt (ActivityView.cpp:1053-1055).
      const row = e.local as LocalMessage;
      items.push({ id: `local-${row.id}`, kind: "user", at: "", body: row.text, attachments: attachments(e), local: localItem(row, summary) });
      i++; continue;
    }
    if (role) {
      let receipt: string | undefined;
      if (role === "user") {
        let accepted = e.type !== "UserPromptQueued";
        for (let j = i + 1; !accepted && j < events.length; j++) {
          const next = events[j];
          if ((next.type === "UserPromptSubmit" || next.type === "TurnStarted") && next.detail === e.detail) accepted = true;
        }
        receipt = accepted ? tr("Sent") : tr("Queued at this time");
      }
      // An answer to a question card, or a message holding Codex replies (ActivityView.cpp:1036-1038).
      const replies = role === "user" && e.question_replies?.length ? e.question_replies : undefined;
      // Qt's order: a queued prompt first, then an answer (ActivityView.cpp:1036-1038).
      const label = role !== "user" ? undefined : e.type === "UserPromptQueued" ? tr("You (queued)")
        : e.type === "QuestionAnswered" || replies ? tr("You (answer)") : undefined;
      items.push({ id, kind: role === "user" ? "user" : "agent", at, body: e.detail ?? "",
        echoBody: typeof e.attachment_echo === "string" ? e.attachment_echo : undefined,
        attachments: attachments(e), receipt, label, replies });
      i++; continue;
    }
    // Adjacent operational events form one group; messages and attention stay boundaries.
    const group = [e];
    i++;
    while (i < events.length && group.length < GROUP_LIMIT && events[i].type !== "AgentThinking" && events[i].type !== "SessionCleared"
      && !messageRole(events[i]) && !attention(e) && !attention(events[i])) group.push(events[i++]);
    const tools: string[] = [];
    let calls = 0;
    for (const g of group) {
      if (g.tool && !tools.includes(g.tool)) tools.push(g.tool);
      if (g.type === "PreToolUse") calls++;
    }
    const last = group[group.length - 1];
    const lines = group.map((g) => `${eventTitle(g.type)}${g.tool ? ` / ${g.tool}` : ""}${childEvent(g) ? tr(" (subagent %1)", g.agent_id) : ""}  ${preview(g.detail)}`);
    const shownTools = tools.slice(0, 3).join(", ") + (tools.length > 3 ? " …" : "");
    if (group.length === 1) {
      items.push({ id, kind: "tool", at: timeText(last.at), title: `${eventTitle(e.type)}${e.tool ? ` / ${e.tool}` : ""}`, body: preview(e.detail), lines, warn: attention(e) });
    } else {
      const title = calls > 0 ? tr("%1, %2 events", calls === 1 ? tr("1 tool call") : tr("%1 tool calls", calls), group.length) : tr("%1 activity events", group.length);
      items.push({ id, kind: "tools", at: timeText(last.at), title, body: shownTools || preview(last.detail), lines, warn: group.some(attention) });
    }
  }
  return items;
}

/** One open Activity only. Remember at most 50 local receipts and 1000 small
 * journal/display-key bindings; never retain another session's transcript.
 * Rust may drop its local row before the corresponding activity batch arrives. */
export function createTranscriptProjection() {
  const recent = new Map<string, LocalMessage>(), bindings = new Map<string, string>();
  const trim = <T>(map: Map<string, T>, limit: number) => { while (map.size > limit) map.delete(map.keys().next().value!); };
  return {
    reset() { recent.clear(); bindings.clear(); },
    build(given: readonly ActivityEvent[], summary?: Record<string, unknown>, local: readonly LocalMessage[] = []): TranscriptItem[] {
      for (const row of local) { recent.set(row.id, row); }
      trim(recent, 50);
      const present = new Set(local.map(row => row.id));
      const bound = new Set(bindings.values());
      const missing = [...recent.values()].filter(row => !present.has(row.id) && row.status !== "error" && row.source !== "hgs_voice");
      // A confirmed local receipt can leave Rust's view one batch before its
      // journal event. Keep that one row through the handoff, without actions.
      const waiting = missing.filter(row => !bound.has(`local-${row.id}`));
      const bindingOnly = missing.filter(row => bound.has(`local-${row.id}`));
      const provider = mergeAttachmentMessages(mergeProvider(mergeMessages(given, summary), summary), summary);
      const aliased = provider.map(event => {
        const id = bindings.get(eventKey(event));
        return id ? { ...event, activity_display_key: id } : event;
      });
      let events = mergeLocal(aliased, [...local, ...waiting], bindingOnly);
      events = events.map(event => {
        const key = typeof event.activity_journal_key === "string" ? event.activity_journal_key : eventKey(event);
        if (typeof event.activity_display_key === "string") bindings.set(key, event.activity_display_key);
        const id = bindings.get(key);
        return id ? { ...event, activity_display_key: id } : event;
      });
      trim(bindings, 1000);
      return transcriptItems(mergeAttachmentMessages(events, summary), summary, !provider.length);
    },
  };
}
