import type { ActivityEvent, ComposerView, EventAttachment, LocalMessage, QueueView } from "../../types";
import type { AttachmentScope } from "../../store/activitySource";
import type { FollowTarget } from "../../store/activity";
import type { ActivityItem, Session } from "./types";
import { attachments } from "./transcript";

interface Entry { text: string; exact: boolean }
const plainTerminal = (text: string) => text
  .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "")
  .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
  .replace(/\r\n/g, "\n").replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "");
const canonical = (text: string) => text.replace(/\r\n/g, "\n").trim();

/** Remove terminal chrome only; do not join wrapped lines or guess lost text. */
export function queueEntries(text: string): Entry[] {
  const lines = plainTerminal(text).split("\n");
  const entries: Entry[] = [];
  let current: { lines: string[]; indent: number; exact: boolean } | undefined;
  let codex = false;
  let ambiguous = false;
  const finish = () => {
    if (!current) return;
    const text = current.lines.join("\n").trim();
    if (text) entries.push({ text, exact: current.exact && !/(?:…|\.{3})\s*$/.test(text) });
    current = undefined;
  };
  let last = lines.length - 1;
  while (last >= 0 && !lines[last].trim()) last--;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\u2022 (?:Messages to be submitted (?:after next tool call|at end of turn)|Queued follow-up inputs)/.test(line)) {
      finish(); codex = true; continue;
    }
    if (codex && ((!current && line.trim() === "(press esc to interrupt and send immediately)")
      || (i === last && line.trim() === "shift+← edit last queued message"))) continue;
    const marker = /^( *)([❯↳›])( ?)(.*)$/.exec(line);
    if (marker && marker[1].length <= 2) {
      // An indented Claude/Kimi prompt can also be literal continuation text.
      // Keep a readable split, but do not adopt receipts from an ambiguous span.
      if (marker[1].length && marker[2] !== "↳") ambiguous = true;
      // Native trim removes indentation only from the first prompt. Later
      // Codex arrows keep two spaces; nested body arrows keep four or more.
      finish(); current = { lines: [marker[4]], indent: marker[1].length + 1 + marker[3].length, exact: true }; continue;
    }
    if (current) current.lines.push(line.startsWith(" ".repeat(current.indent)) ? line.slice(current.indent) : line);
    else if (!codex && line.trim()) current = { lines: [line], indent: 0, exact: false };
  }
  finish();
  return ambiguous ? entries.map(entry => ({ ...entry, exact: false })) : entries;
}

/** An unknown host path is display text only, never an attachment reference. */
export function sanitizedQueueText(text: string): string {
  return text
    .replace(/(\[(?:Image|File) #[1-9][0-9]{0,8}\]) ?\((?:Inspect the attached image at|Read the attached file at) (?:"(?:[^"\\]|\\.)*"|[^)\n]*) ?\)?/g, "$1")
    .replace(/(\[(?:Image|File) #[1-9][0-9]{0,8}\]) (?:Inspect the attached image at|Read the attached file at) (?:"(?:[^"\\]|\\.)*"|[^\n]*)/g, "$1")
    .replace(/(Inspect the attached image at|Read the attached file at) (?:"(?:[^"\\]|\\.)*"|[^\n]*)/g,
      (_whole, instruction: string) => instruction.startsWith("Inspect") ? "[Image]" : "[File]");
}

interface Receipt { id: string; submitted: string; body: string; files: ActivityItem["attachments"]; metadata: string }
function receipt(id: unknown, submitted: unknown, body: unknown, files: unknown): Receipt | undefined {
  if (typeof id !== "string" || !id || typeof submitted !== "string" || !canonical(submitted) || typeof body !== "string" || !Array.isArray(files) || files.length > 8) return;
  if (!files.every((file, index) => file && typeof file === "object" && file.request_id === id && file.index === index
    && typeof file.name === "string" && typeof file.mime === "string" && typeof file.reference === "string"
    && typeof file.bytes === "number" && Number.isFinite(file.bytes) && file.bytes >= 0)) return;
  const metadata = JSON.stringify((files as EventAttachment[]).map(f => ({ request_id: f.request_id, index: f.index,
    name: f.name, mime: f.mime, bytes: f.bytes, reference: f.reference })));
  return { id, submitted: canonical(submitted), body, files: attachments({ attachments: files as EventAttachment[] }), metadata };
}

/** Full, unique receipt equality only; the native queue's push-prefix test is NOT a display match. */
export function queuedMessages(queue: QueueView, local: readonly LocalMessage[], summary?: Record<string, unknown>): ActivityItem[] {
  const entries = queueEntries(queue.text);
  const receipts = new Map<string, Receipt[]>();
  const add = (value: Receipt | undefined) => {
    if (!value) return;
    const same = receipts.get(value.submitted) ?? [];
    // The same receipt can briefly exist in both the local and host projection.
    if (!same.some((old) => old.id === value.id && old.submitted === value.submitted
      && canonical(old.body) === canonical(value.body) && old.metadata === value.metadata)) same.push(value);
    receipts.set(value.submitted, same);
  };
  if (queue.run_id && queue.conversation_id) {
    for (const row of local.slice(-50)) {
      if (row.status === "sent" && !row.uncertain && row.id === row.message_id
        && row.run_id === queue.run_id && row.conversation_id === queue.conversation_id && !row.source) {
        add(receipt(row.message_id, row.submitted_text, row.text, row.attachments));
      }
    }
    if (summary?.run_id === queue.run_id && summary.conversation_id === queue.conversation_id && Array.isArray(summary.attachment_messages)) {
      for (const event of (summary.attachment_messages as ActivityEvent[]).slice(-100)) {
        if (event?.source === "hgs_delivery" && !event.agent_id) add(receipt(event.message_id, event.submitted_text, event.detail, event.attachments));
      }
    }
  }
  const occurrences = new Map<string, number>();
  for (const entry of entries) occurrences.set(entry.text, (occurrences.get(entry.text) ?? 0) + 1);
  return entries.map((entry, index) => {
    const candidates = receipts.get(entry.text) ?? [];
    const exact = entry.exact && occurrences.get(entry.text) === 1 && candidates.length === 1 ? candidates[0] : undefined;
    return { id: `queue-${index}`, kind: "user", at: "", body: exact?.body ?? sanitizedQueueText(entry.text), attachments: exact?.files };
  });
}

/** Local and host receipt sources must still belong to this queue's exact
 * selected main conversation. A late composer or follow cannot supply files. */
export function queuePresentation(queue: QueueView, session: Session | undefined,
  view: Pick<ComposerView, "key" | "subagent" | "local"> | undefined, target: FollowTarget | undefined,
  summary: Record<string, unknown> | undefined, child: boolean) {
  const current = session && !child && !session.archiveId && !view?.subagent
    && view?.key === `${session.computer}\n${session.name}`
    && !!queue.run_id && !!queue.conversation_id
    && session.runId === queue.run_id && session.conversationId === queue.conversation_id;
  const host = current && target?.id === session.key && target.computer === session.computer
    && target.key === session.catalogKey ? summary : undefined;
  const scope: AttachmentScope | undefined = current ? { computer: session.computer, name: session.name,
    conversation_id: queue.conversation_id!, archive_id: null } : undefined;
  return { items: queuedMessages(queue, current ? view.local : [], host), scope };
}
