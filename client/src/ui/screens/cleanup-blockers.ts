/**
 * Why a working folder was kept, grouped for the Remove working folders review
 * (zerus-l1ryn). Presentation only: every next step names an existing explicit
 * action (Archive from the session menu, Check again, Force stop, Remove
 * anyway or a consent the native review already requires). Nothing here
 * grants removal, stops a program or replays an unknown result.
 */
import type { CleanupRow } from "../../store/terminationCleanup";

/** The fresh native review row of one folder (`hgs worktrees inspect`). */
export interface BlockerReview {
  reasons?: { code: string; message: string }[];
  sessions?: unknown[];
  processes?: { pid: number; command?: string; label?: string }[];
  process_count?: number;
  unreadable_processes?: unknown[];
  untracked?: number;
  changes?: number;
}

export type BlockerRow = Pick<CleanupRow, "status" | "reason" | "sessions" | "processes" | "can_force" | "can_remove_anyway" | "ready">;

export type NextStep =
  | { kind: "unknown" }
  | { kind: "archive"; sessions: string[] }
  | { kind: "force" }
  | { kind: "close_programs" }
  | { kind: "accept_unchecked" }
  | { kind: "remove_anyway" }
  | { kind: "check_hidden" }
  | { kind: "discard" }
  | { kind: "review_files" }
  | { kind: "none" };

export interface Blockers {
  sessions: string[];
  programs: { pid: number; label: string }[];
  /** All programs the review counted, including any beyond the bounded list. */
  programCount: number;
  /** Programs whose folders this user cannot read; zero when the review did not count them. */
  hidden: number;
  untracked: number;
  changes: number;
  /** Reasons that belong to no group, as the native review worded them. */
  other: string[];
  next: NextStep;
}

const GROUPED = new Set(["sessions", "processes", "untracked", "changes"]);

const unique = <T>(values: T[], key: (value: T) => string) => values.filter((value, i, all) => all.findIndex((other) => key(other) === key(value)) === i);
const count = (value: unknown) => (typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : 0);

/** Groups one folder's blockers and picks the first next step that applies. */
export function cleanupBlockers(rows: readonly BlockerRow[], review?: BlockerReview): Blockers {
  const unknown = rows.some((row) => row.status === "unknown");
  const sessions = unique(
    (review?.sessions ?? rows.flatMap((row) => row.sessions ?? [])).filter((name): name is string => typeof name === "string" && name.length > 0),
    (name) => name,
  );
  const programs = unique(
    review?.processes?.map((p) => ({ pid: p.pid, label: p.command || p.label || "Program" })) ?? rows.flatMap((row) => row.processes ?? []),
    (p) => `${p.pid}\n${p.label}`,
  );
  const programCount = Math.max(count(review?.process_count), programs.length);
  const hidden = Array.isArray(review?.unreadable_processes) ? review.unreadable_processes.length : 0;
  const untracked = count(review?.untracked);
  const changes = count(review?.changes);
  // Without the structured review only the recorded text exists: show it
  // whole unless the recorded sessions or programs already explain it.
  const other = review?.reasons
    ? unique(review.reasons.filter((r) => !GROUPED.has(r.code) && !(r.code === "processes_unavailable" && hidden > 0)).map((r) => r.message).filter(Boolean), (m) => m)
    : sessions.length || programs.length ? [] : unique(rows.flatMap((row) => row.reason.split("\n")).map((line) => line.trim()).filter(Boolean), (m) => m);
  const ready = rows.some((row) => row.ready && row.status === "ready");
  const next: NextStep = unknown ? { kind: "unknown" }
    : sessions.length ? { kind: "archive", sessions }
    : rows.some((row) => row.can_force) ? { kind: "force" }
    : programs.length ? { kind: "close_programs" }
    : hidden ? (ready ? { kind: "accept_unchecked" } : rows.some((row) => row.can_remove_anyway) ? { kind: "remove_anyway" } : { kind: "check_hidden" })
    : untracked || changes ? (ready ? { kind: "discard" } : { kind: "review_files" })
    : { kind: "none" };
  return { sessions, programs, programCount, hidden, untracked, changes, other, next };
}
