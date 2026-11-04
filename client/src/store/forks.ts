import { createEffect, createSignal, untrack } from "solid-js";
import type { Action, Details, Outcome } from "../types";
import type { Session } from "../ui/sessions/types";
import { snapshotOf, type ActionRow } from "../ui/sessions/actions-ui";
import { FORK_CHANGED, forkAvailable, forkName, forkProblem, forkTag, sameForkSource } from "../ui/sessions/fork-ui";
import { tr } from "../i18n";

type Neighbor = Pick<Session, "key" | "computer" | "name">;
interface Pending { name: string; source: ActionRow; group: string; before?: Neighbor; succeeded: boolean }
export interface ForkDeps {
  rows(): readonly Session[];
  current(key: string): Session | undefined;
  online(computer: string): boolean;
  busy(row: ActionRow): boolean;
  inspect(row: ActionRow): Promise<Details | undefined>;
  warning(computer: string): Promise<string | null>;
  perform(row: ActionRow, action: Action): Promise<Outcome>;
  groupOf(key: string): string;
  defaultGroup(): string;
  hasGroup(group: string): boolean;
  next(row: ActionRow, group: string): Neighbor | undefined;
  place(row: Session, group: string, before?: Session): void;
  select(row: Session): void;
  notify(tone: "info" | "error", text: string, sticky?: boolean): void;
}

/** One fork and one catalog follow-up, as Qt's m_pending/m_forkKey. No timer,
 * background read, durable replay or second launch path. */
export function createForks(deps: ForkDeps) {
  const [working, setWorking] = createSignal(false);
  const [pending, setPending] = createSignal<Pending>();
  const busy = () => working() || !!pending();
  const available = (row: ActionRow, details?: Details) => forkAvailable(row, details, deps.online(row.computer), busy() || deps.busy(row));
  const problem = (row: ActionRow, tag: string) => forkProblem(row, tag, deps.rows());
  const suggested = (row: ActionRow) => forkTag(row, deps.rows());

  createEffect(() => {
    const wait = pending();
    if (!wait?.succeeded) return;
    const row = deps.rows().find((s) => s.computer === wait.source.computer && s.name === wait.name && s.state === "running"
      && !!s.runId && !!s.conversationId && s.conversationId !== wait.source.conversationId);
    if (!row) return;
    untrack(() => {
      setPending(undefined);
      const group = deps.hasGroup(wait.group) ? wait.group : deps.defaultGroup();
      const before = wait.before ? deps.current(wait.before.key) : undefined;
      const kept = before && before.name === wait.before?.name && before.computer === wait.before.computer && deps.groupOf(before.key) === group ? before : undefined;
      deps.place(row, group, kept);
      deps.select(row);
    });
  });

  async function submit(source: ActionRow, shown: Details, tag: string, signal: AbortSignal,
    confirm: (text: string) => Promise<boolean>, dispatched: () => void): Promise<void> {
    if (busy() || deps.busy(source) || signal.aborted || problem(source, tag)) return;
    setWorking(true);
    let delivered = false;
    try {
      const valid = () => {
        const row = deps.current(source.key);
        return row && sameForkSource(source, row) && deps.online(row.computer) && !deps.busy(row) && !signal.aborted && !problem(source, tag);
      };
      if (!valid()) { if (!signal.aborted) deps.notify("error", tr(FORK_CHANGED), true); return; }
      // Confirm against a fresh inspection of the source named when the dialog
      // opened. A new run/conversation or a capability change never forks it.
      const details = await deps.inspect(source);
      if (signal.aborted) return;
      if (!valid() || !forkAvailable(source, details, true, false) || details?.run_id !== shown.run_id || details.conversation_id !== shown.conversation_id) {
        deps.notify("error", tr(FORK_CHANGED), true); return;
      }
      const warning = await deps.warning(source.computer);
      if (signal.aborted || (warning && !(await confirm(warning)))) return;
      if (!valid()) { if (!signal.aborted) deps.notify("error", tr(FORK_CHANGED), true); return; }
      const group = deps.groupOf(source.key), before = deps.next(source, group);
      const wait: Pending = { source: snapshotOf(source), name: forkName(source, tag), group,
        before: before && { key: before.key, computer: before.computer, name: before.name }, succeeded: false };
      setPending(wait);
      delivered = true;
      dispatched();
      deps.notify("info", tr("Forking session on %1…", source.machine));
      const out = await deps.perform({ ...source, runId: details.run_id || source.runId, conversationId: details.conversation_id },
        { kind: "fork", tag, archive_id: source.archiveId ?? null });
      if (out.outcome === "done") setPending({ ...wait, succeeded: true });
      else { setPending(undefined); deps.notify("error", out.detail, true); }
    } catch (error) {
      setPending(undefined);
      if (delivered || !signal.aborted) deps.notify("error", error instanceof Error ? error.message : String(error), true);
    } finally { setWorking(false); }
  }
  return { busy, working, pending, available, problem, suggested, submit };
}
