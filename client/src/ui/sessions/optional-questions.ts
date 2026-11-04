import type { Session } from "./types";

/** Qt's optionalQuestionCount: preserve source facts while hiding stale chips. */
export function optionalQuestionCount(s: Pick<Session, "state" | "processState" | "optionalQuestions">, online: boolean): number {
  const count = s.optionalQuestions;
  return online && s.state === "running" && s.processState !== "exited"
    && typeof count === "number" && Number.isInteger(count) && count > 0 && count <= 1e9 ? count : 0;
}

export const optionalQuestionCountText = (count: number): string => count > 99 ? "99+" : count > 0 ? String(count) : "";
