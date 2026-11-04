import type { Session, Subagent } from "./types";

export type SubagentMap = Record<string, Record<string, unknown>>;

const text = (value: unknown) => typeof value === "string" ? value : "";
const simplified = (value: unknown) => text(value).trim().replace(/\s+/g, " ").slice(0, 240);
export const childState = (child: Record<string, unknown>) => text(child.display_state) || text(child.state);

/** SessionPresentation::inactiveChild, including grouped count evidence. */
export function childInactive(child: Record<string, unknown>, countsComplete = false): boolean {
  const state = childState(child);
  const inactive = ["finished", "idle", "ready", "error"].includes(state);
  if (state && !inactive) return false;
  if (child.group === true) return countsComplete && typeof child.active_count === "number" && child.active_count === 0;
  return inactive;
}

const validId = (id: string) => id.length > 0 && id.length <= 160 && !/[/\\\u0000-\u001f\u007f]/.test(id);

/** Stable native-child rows, never the hook-profile fallback. */
export function childRows(map: SubagentMap | undefined, countsComplete = false): Subagent[] {
  return Object.entries(map ?? {}).filter(([id, child]) => validId(id) && child && typeof child === "object" && !Array.isArray(child))
    .sort(([a], [b]) => a.localeCompare(b)).map(([id, child]) => {
      const raw = childState(child);
      const state = raw === "working" ? "working" : ["approval", "input", "attention"].includes(raw) ? "attention" : raw === "error" ? "error" : raw === "paused" ? "paused" : "ready";
      const label = typeof child.label === "string" ? child.label : typeof child.name === "string" ? child.name : "";
      const detail = typeof child.detail === "string" ? child.detail : child.task;
      return {
        id,
        name: label || id,
        state,
        summary: simplified(detail),
        inactive: childInactive(child, countsComplete),
        group: child.group === true,
      };
    });
}

/** Qt's selected-inspection roster override, only for the same fresh conversation. */
export function rosterFor(row: Session, details?: Record<string, unknown>): Subagent[] {
  const inspected = details?.subagents;
  const fresh = (typeof details?.last_event_at === "number" ? details.last_event_at : 0) >= (row.updatedAt ?? 0);
  const same = !row.conversationId || details?.conversation_id === row.conversationId;
  if (inspected && typeof inspected === "object" && !Array.isArray(inspected) && fresh && same) {
    return childRows(inspected as SubagentMap, details?.subagent_counts_complete === true);
  }
  return row.subagents ?? [];
}
