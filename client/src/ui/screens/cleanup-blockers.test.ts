import { describe, expect, it } from "vitest";
import { cleanupBlockers, type BlockerRow, type BlockerReview } from "./cleanup-blockers";

const row = (over: Partial<BlockerRow> = {}): BlockerRow => ({ status: "kept", reason: "", ready: false, ...over });
const hiddenReason = { code: "processes_unavailable", message: "Zerus cannot check whether some running programs use this folder (7 processes with hidden paths)." };

describe("cleanup blockers (zerus-l1ryn)", () => {
  it("groups other Zerus sessions and makes archiving them the next step", () => {
    const review: BlockerReview = {
      sessions: ["codex/balance-review", "kimi/doge-docs-a", "codex/balance-review", 7],
      reasons: [{ code: "sessions", message: "Zerus sessions use this folder: codex/balance-review. Archive them first." }],
    };
    const blockers = cleanupBlockers([row()], review);
    expect(blockers.sessions).toEqual(["codex/balance-review", "kimi/doge-docs-a"]);
    expect(blockers.next).toEqual({ kind: "archive", sessions: ["codex/balance-review", "kimi/doge-docs-a"] });
    // The grouped reason is not repeated as loose text.
    expect(blockers.other).toEqual([]);
  });

  it("asks to close known programs when only programs block, and never offers to stop them itself", () => {
    const review: BlockerReview = {
      processes: [{ pid: 41, command: "python3" }, { pid: 42, label: "editor" }, { pid: 43 }],
      process_count: 5,
      reasons: [{ code: "processes", message: "3 running processes use this folder: python3 (41). Close them first." }],
    };
    const blockers = cleanupBlockers([row()], review);
    expect(blockers.programs).toEqual([{ pid: 41, label: "python3" }, { pid: 42, label: "editor" }, { pid: 43, label: "Program" }]);
    expect(blockers.programCount).toBe(5);
    expect(blockers.next).toEqual({ kind: "close_programs" });
    // An incomplete original stop has its own existing Force stop path.
    expect(cleanupBlockers([row({ can_force: true })], review).next).toEqual({ kind: "force" });
  });

  it("puts sessions first when sessions, programs, hidden paths and untracked files all block", () => {
    const review: BlockerReview = {
      sessions: ["codex/balance-review", "kimi/doge-docs-a", "kimi/doge-docs-b"],
      processes: [{ pid: 101, command: "codex" }, { pid: 102, command: "python3" }, { pid: 103, command: "hgs" }],
      process_count: 3,
      unreadable_processes: [{}, {}, {}, {}, {}, {}, {}],
      untracked: 1,
      reasons: [
        { code: "processes", message: "3 running processes use this folder." },
        hiddenReason,
        { code: "sessions", message: "Zerus sessions use this folder." },
        { code: "untracked", message: "1 untracked file." },
        { code: "locked", message: "Locked by Git." },
      ],
    };
    const blockers = cleanupBlockers([row({ can_force: true })], review);
    expect(blockers.sessions).toHaveLength(3);
    expect(blockers.programs.map((p) => p.label)).toEqual(["codex", "python3", "hgs"]);
    expect(blockers.hidden).toBe(7);
    expect(blockers.untracked).toBe(1);
    // The hidden-path reason is explained by the programs group; others stay.
    expect(blockers.other).toEqual(["Locked by Git."]);
    expect(blockers.next.kind).toBe("archive");
  });

  it("offers nothing actionable for an unknown removal result", () => {
    const review: BlockerReview = { sessions: ["kimi/doge-docs-a"], processes: [{ pid: 9, command: "codex" }], untracked: 2 };
    const blockers = cleanupBlockers([row({ status: "unknown", can_force: true, can_remove_anyway: true })], review);
    expect(blockers.next).toEqual({ kind: "unknown" });
    expect(cleanupBlockers([row(), row({ status: "unknown" })], review).next).toEqual({ kind: "unknown" });
  });

  it("points hidden-path programs at the existing five-minute consent or manual review only", () => {
    const review: BlockerReview = { unreadable_processes: [{}, {}], reasons: [hiddenReason] };
    expect(cleanupBlockers([row({ status: "ready", ready: true })], review).next).toEqual({ kind: "accept_unchecked" });
    expect(cleanupBlockers([row({ can_remove_anyway: true })], review).next).toEqual({ kind: "remove_anyway" });
    expect(cleanupBlockers([row()], review).next).toEqual({ kind: "check_hidden" });
    // An unreadable sample without a count keeps the native wording instead.
    const uncounted = cleanupBlockers([row()], { reasons: [{ code: "processes_unavailable", message: "Process review expired." }] });
    expect(uncounted.hidden).toBe(0);
    expect(uncounted.other).toEqual(["Process review expired."]);
    expect(uncounted.next).toEqual({ kind: "none" });
  });

  it("explains files that removal would discard", () => {
    const review: BlockerReview = { untracked: 1, changes: 2, reasons: [{ code: "untracked", message: "1 untracked file." }, { code: "changes", message: "2 changed files not committed." }] };
    expect(cleanupBlockers([row()], review)).toMatchObject({ untracked: 1, changes: 2, other: [], next: { kind: "review_files" } });
    expect(cleanupBlockers([row({ status: "ready", ready: true })], review).next).toEqual({ kind: "discard" });
  });

  it("falls back to the recorded row without a fresh review", () => {
    const recorded = cleanupBlockers([row({ reason: "Zerus sessions use this folder: a, b.\nLocked by Git.", sessions: ["a", "b"], processes: [{ pid: 5, label: "vim" }] })]);
    expect(recorded.sessions).toEqual(["a", "b"]);
    expect(recorded.programs).toEqual([{ pid: 5, label: "vim" }]);
    expect(recorded.other).toEqual([]);
    const plain = cleanupBlockers([row({ reason: "Locked by Git.\nLocked by Git." })]);
    expect(plain.other).toEqual(["Locked by Git."]);
    expect(plain.next).toEqual({ kind: "none" });
  });
});
