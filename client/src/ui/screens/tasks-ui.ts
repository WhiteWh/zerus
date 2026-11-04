import type { Session } from "../sessions/types";
import { tr, trNoop } from "../../i18n";

export const STAGES = ["review", "active", "todo", "backlog", "done"] as const;
export type Stage = typeof STAGES[number];
export const STAGE_LABEL: Record<Stage, string> = { review: trNoop("In review"), active: trNoop("In progress"), todo: trNoop("To do"), backlog: trNoop("Backlog"), done: trNoop("Done") };
/** A stage as shown; an unknown stage stays as reported. */
export const stageLabel = (stage: string): string => (stage in STAGE_LABEL ? tr(STAGE_LABEL[stage as Stage]) : stage);
export const STAGE_ICON: Record<Stage, string> = { review: "inbox", active: "play", todo: "list-todo", backlog: "clock", done: "check" };
export const RECENT_DONE = 7 * 24 * 60 * 60 * 1000;
export const TASK_LIMIT = 10_000, TASK_PAGE = 50;
export const object = (v: unknown): Record<string, unknown> => v && typeof v === "object" && !Array.isArray(v) ? v as Record<string, unknown> : {};
export const text = (v: unknown) => typeof v === "string" ? v : "";
const number = (v: unknown) => typeof v === "number" && Number.isFinite(v) ? v : 0;
export interface TaskRef { host: string | null; name: string; id: string; known: boolean; machine?: string }
export interface Task {
  uuid: string; id: string; ref: string; title: string; text: string; stage: string; summary: string; cancelReason: string; historyRevision: string; dispatchId: string;
  createdAt: number; stageAt: number; cancelledAt: number; queue: number; writable: boolean;
  target: TaskRef; executor: TaskRef; author: TaskRef; targetKind: string; worker: string; attachments: number;
}
const reference = (v: unknown): TaskRef => {
  const o = object(v);
  return { host: o.host === null ? null : text(o.host), name: text(o.session), id: text(o.session_id), machine: text(o.machine), known: o.host !== null && o.resolved !== false };
};
export function taskItem(value: unknown, queue = 0): Task {
  const t = object(value), target = object(t.target), author = object(t.author);
  return { uuid: text(t.uuid), id: text(t.id), ref: text(t.ref), title: text(t.title) || text(t.text).split("\n", 1)[0],
    text: text(t.text), stage: text(t.stage), summary: text(t.summary), cancelReason: text(t.cancel_reason), historyRevision: text(t.history_revision), dispatchId: text(t.dispatch_id),
    createdAt: number(t.created_at), stageAt: number(t.stage_at), cancelledAt: number(t.cancelled_at), queue, writable: t.writable !== false,
    target: reference(target.kind === "any" ? undefined : target.parent ?? target), executor: reference(t.session),
    author: reference(author.kind === "agent" ? author : undefined), targetKind: text(target.kind), worker: text(object(t.worker).name),
    attachments: Array.isArray(t.attachments) ? t.attachments.length : 0 };
}
export const sessionIdentity = (s: Session) => s.launchId ? `launch_id:${s.launchId}` : s.runId ? `run_id:${s.runId}` : "";
export const sessionLabel = (r: TaskRef) => {
  const label = r.name.split("/").slice(2).join("/") || r.name.split("/")[1] || r.name;
  return label ? label + (r.host ? ` @${r.host}` : "") : tr("Any session");
};
const queued = (t: Task) => t.stage === "todo" || t.stage === "backlog";
export const executorRef = (t: Task) => queued(t) ? t.target : t.executor;
export const assignmentLabel = (t: Task) => queued(t)
  ? t.targetKind === "new_session" ? tr("New session — create when started")
    : t.targetKind === "subagent" ? `Subagent — coordinate through ${sessionLabel(t.target)}`
    : t.target.name ? sessionLabel(t.target) : tr("Any available session")
  : t.executor.name ? sessionLabel(t.executor) : "Unconfirmed";
export function assignmentText(t: Task, main?: Session): string {
  if (!queued(t)) return `${t.stage === "active" ? "Executor" : tr("Last executor")}: ${t.executor.name ? sessionLabel(t.executor) : "Unconfirmed"}`;
  const assigned = !t.target.name ? tr("Any available session") : sessionLabel(t.target);
  const label = t.targetKind === "new_session" ? tr("New session, created when started") : t.targetKind === "subagent" ? tr("Subagent coordinated through %1", assigned) : main && t.target.id === sessionIdentity(main) && t.target.host === main.computer ? tr("Main session: %1", assigned) : assigned;
  return `Planned: ${label}`;
}
export function belongsTo(t: Task, s: Session): boolean {
  const r = executorRef(t);
  return r.known && r.host === s.computer && (r.id ? r.id === sessionIdentity(s) : !!r.name && r.name === s.name);
}
export function boardRows(tasks: readonly Task[], stage: Stage, only: Session | undefined, now: number, older = false): Task[] {
  return tasks.filter((t) => t.stage === stage && (!only || belongsTo(t, only) || (queued(t) && !t.target.name))
    && (stage !== "done" || (now - t.stageAt > RECENT_DONE) === older));
}
/** Keep an older page anchored when newer tasks arrive above it. */
export function taskPageOffset(previous: readonly Task[], next: readonly Task[], at: number): number {
  if (!at) return 0;
  const indexes = new Map(next.map((t, i) => [t.uuid, i]));
  for (const task of previous.slice(at, at + TASK_PAGE)) {
    const index = indexes.get(task.uuid); if (index !== undefined) return index;
  }
  return Math.max(0, Math.min(at, Math.floor((next.length - 1) / TASK_PAGE) * TASK_PAGE));
}
/** Resolve immutable identities once per catalog change, never scan sessions for each row. */
export function taskSessions(sessions: readonly Session[]) {
  const byId = new Map<string, Session | null>(), byName = new Map<string, Session | null>();
  for (const s of sessions) {
    const name = JSON.stringify([s.computer, s.name]), id = JSON.stringify([s.computer, sessionIdentity(s)]);
    byName.set(name, byName.has(name) ? null : s);
    if (sessionIdentity(s)) byId.set(id, byId.has(id) ? null : s);
  }
  return (r: TaskRef) => r.host === null ? undefined : (r.id ? byId.get(JSON.stringify([r.host, r.id]))
    : r.known ? byName.get(JSON.stringify([r.host, r.name])) : undefined) ?? undefined;
}
export function projectTask(t: Task, find: ReturnType<typeof taskSessions>): Task {
  const resolve = (r: TaskRef): TaskRef => {
    if (!r.id) return r;
    const s = find(r); return s ? { ...r, name: s.name, known: true } : { ...r, known: false };
  };
  return { ...t, target: resolve(t.target), executor: resolve(t.executor), author: resolve(t.author) };
}
