import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { desktopService } from "./desktopService";

/**
 * The background service's submitted question routes of one local session
 * (zerus-sa9qn.33), as Activity lines: the newest 20 where it is the child or
 * the parent. Read-only: nothing here sends, answers or retries anything, and
 * refused or unknown routes are not shown. The child's words appear only as
 * the route's own preview of at most 300 characters; a parent's line also
 * holds the exact text it received, so its own message folds into the line.
 */
export interface RouteActivity {
  requestId: string;
  /** When the service reserved the route, in seconds like journal events. */
  at: number;
  role: "child" | "parent";
  child: { name: string; reference: string; sessionId: string };
  parent: { name: string; sessionId: string };
  question: { kind: string; questions: number; options: number; preview: string };
  /** The parent's exact received text; the child's line has none. */
  text?: string;
}

export const ROUTE_LIMIT = 20;
const KINDS = new Set(["single-choice", "multiple-choice", "free-text", "multi-part"]);
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const NIL = "00000000-0000-0000-0000-000000000000";
const canonical = (v: unknown): v is string => typeof v === "string" && UUID.test(v) && v !== NIL;
const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const exact = (v: unknown, keys: readonly string[]): v is Record<string, unknown> =>
  record(v) && Object.keys(v).length === keys.length && keys.every((k) => k in v);
const bounded = (v: unknown, bytes: number): v is string => typeof v === "string" && v.length > 0 && new TextEncoder().encode(v).length <= bytes;
const count = (v: unknown, min: number): v is number => typeof v === "number" && Number.isInteger(v) && v >= min && v <= 0xffffffff;
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

function line(v: unknown, sessionId: string): RouteActivity | undefined {
  const parentRole = record(v) && v.role === "parent";
  const keys = ["request_id", "at", "role", "child", "parent", "question", ...(parentRole ? ["text"] : [])];
  if (!exact(v, keys) || (v.role !== "child" && !parentRole)) return undefined;
  const { child, parent, question } = v;
  if (!canonical(v.request_id) || !count(v.at, 1)) return undefined;
  if (!exact(child, ["name", "reference", "session_id"]) || !bounded(child.name, 512) || !canonical(child.session_id)
    || !bounded(child.reference, 1024) || !child.reference.startsWith("zerus://session?")) return undefined;
  if (!exact(parent, ["name", "session_id"]) || !bounded(parent.name, 512) || !canonical(parent.session_id)) return undefined;
  if (child.session_id === parent.session_id || (parentRole ? parent.session_id : child.session_id) !== sessionId) return undefined;
  if (!exact(question, ["kind", "questions", "options", "preview"]) || typeof question.kind !== "string" || !KINDS.has(question.kind)
    || !count(question.questions, 1) || !count(question.options, 0) || typeof question.preview !== "string"
    || [...question.preview].length > 300 || CONTROL.test(question.preview)) return undefined;
  if (parentRole && !bounded(v.text, 2048)) return undefined;
  return {
    requestId: v.request_id, at: v.at / 1000, role: parentRole ? "parent" : "child",
    child: { name: child.name, reference: child.reference, sessionId: child.session_id },
    parent: { name: parent.name, sessionId: parent.session_id },
    question: { kind: question.kind, questions: question.questions, options: question.options, preview: question.preview },
    ...(parentRole ? { text: v.text as string } : {}),
  };
}

/** The native reply's lines about `sessionId`, or none unless every line is exact. */
export function parseRouteActivity(value: unknown, sessionId: string): RouteActivity[] {
  if (!exact(value, ["routes"]) || !Array.isArray(value.routes) || value.routes.length > ROUTE_LIMIT) return [];
  const lines = value.routes.map((route) => line(route, sessionId));
  return lines.every((l): l is RouteActivity => !!l) ? lines : [];
}

/**
 * The durable ID whose lines the Activity shows: the live main Activity of a
 * session on this computer that the follow targets, never a subagent's, an
 * archive's or a remote row's.
 */
export function routeSessionOf(row: { computer: string; key: string; sessionId?: string } | undefined,
  shown: { archive: boolean; subagent: boolean; target: string | undefined }): string | undefined {
  return row && !shown.archive && !shown.subagent && row.computer === "" && shown.target === row.key ? row.sessionId : undefined;
}

/** Only the shown session's lines are kept. */
const [shown, setShown] = createSignal<{ sessionId: string; routes: RouteActivity[] }>();
const EMPTY: RouteActivity[] = [];
/** At most one read per session in this window; a later change reads once at its end. */
export const ROUTE_REFRESH_MS = 3000;
/** A shown child is read again at this pace: its own route adds no activity to it. */
export const ROUTE_FOLLOW_MS = 15000;
let current: string | undefined;
let follow = false;
let last = 0;
let timer: ReturnType<typeof setTimeout> | undefined;
let due = 0;
let reading: string | undefined;
let again = false;

/** The lines of `sessionId` when it is the shown session, else none. */
export function routeActivityOf(sessionId: string | undefined): RouteActivity[] {
  const value = shown();
  return value && sessionId && value.sessionId === sessionId ? value.routes : EMPTY;
}

async function read(sessionId: string): Promise<void> {
  if (reading === sessionId) { again = true; return; }
  reading = sessionId;
  last = Date.now();
  try {
    const reply = await invoke<unknown>("desktop_service_question_route_activity", { session_id: sessionId });
    if (current === sessionId) setShown({ sessionId, routes: parseRouteActivity(reply, sessionId) });
  } catch {
    // A service without this read, or none: no lines, quietly.
    if (current === sessionId) setShown({ sessionId, routes: EMPTY });
  } finally {
    if (reading === sessionId) reading = undefined;
    if (current === sessionId) {
      if (again) schedule(ROUTE_REFRESH_MS);
      else if (follow) schedule(ROUTE_FOLLOW_MS);
      again = false;
    }
  }
}

/** One read of the shown session `window` after the last, unless one is due sooner. */
function schedule(window: number): void {
  const sessionId = current;
  if (!sessionId) return;
  const at = last + window;
  if (timer) {
    if (due <= at) return;
    clearTimeout(timer);
  }
  due = at;
  timer = setTimeout(() => { timer = undefined; if (current === sessionId) void read(sessionId); }, Math.max(0, at - Date.now()));
}

/**
 * Reads the lines of the shown local session: at once for a newly shown
 * session, then at most once per window while its activity changes, and a
 * shown child (`followChild`) also at a slow pace. A remote row, a row
 * without a durable ID, a GUI outside background service mode or no shown
 * session (`undefined`) shows none and reads nothing.
 */
export function refreshRouteActivity(sessionId: string | undefined, followChild = false): void {
  const local = sessionId && canonical(sessionId) && desktopService.enabled() ? sessionId : undefined;
  follow = !!local && followChild;
  if (local !== current) {
    current = local;
    again = false;
    if (timer) { clearTimeout(timer); timer = undefined; }
    setShown(undefined);
    last = 0;
    if (local) void read(local);
    return;
  }
  schedule(ROUTE_REFRESH_MS);
}
