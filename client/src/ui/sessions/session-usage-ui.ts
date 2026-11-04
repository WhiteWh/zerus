// SessionUsage::html, as labelled rows rather than HTML. Values stay separate
// through the table layout; no field separator is added.
import type { Session } from "./types";
import type { Usage } from "../../types";
import { summaryText } from "../lib/usage";
import { tr, trNoop } from "../../i18n";

/** SessionsWindow::renderAccountUsage, including saved live-catalog sessions. */
export function usageStripVisible(page: string, session: Session | undefined, subagent: string | undefined): boolean {
  return page === "sessions" && !!session?.tracked && session.state !== "archived" && !session.archiveId && !subagent;
}

export const footerUsageSummary = (usage: Usage) => !usage.status || usage.status === "unavailable" || usage.status === "offline" ? tr("limits n/a") : summaryText(usage);

export type UsageObject = Record<string, unknown>;
const object = (value: unknown): UsageObject => value && typeof value === "object" && !Array.isArray(value) ? value as UsageObject : {};
const valid = (value: unknown) => typeof value === "number" && Number.isFinite(value) && value >= 0;
const integer = (value: unknown) => valid(value) ? Math.trunc(value as number).toLocaleString() : tr("Not reported");
const fixed = (value: number, digits: number) => value.toLocaleString(undefined, { minimumFractionDigits: digits, maximumFractionDigits: digits });

export interface SessionUsageView {
  status: string;
  rows: { label: string; value: string }[];
  costNote: string;
  context: string;
  footer: string;
}

export function sessionUsageView(raw: unknown): SessionUsageView {
  const usage = object(raw);
  if (usage.status !== "ok") return { status: tr("No native usage reported for this conversation yet."), rows: [], costNote: "", context: "", footer: "" };
  const totals = object(usage.totals);
  const rows: { label: string; value: string }[] = [{ label: tr("Total tokens"), value: integer(totals.total) }];
  const fields: [string, string][] = [
    ["input", trNoop("Input (including cache)")], ["uncached_input", trNoop("Uncached input")], ["cache_read", trNoop("Cache read")], ["cache_write", trNoop("Cache write")],
    ["output", trNoop("Output")], ["reasoning", trNoop("Reasoning (in output)")], ["web_searches", trNoop("Web searches")], ["web_fetches", trNoop("Web fetches")],
  ];
  for (const [key, label] of fields) if (valid(totals[key])) rows.push({ label: tr(label), value: integer(totals[key]) });
  if (valid(usage.requests)) rows.push({ label: tr("Recorded requests"), value: integer(usage.requests) });
  const hasCost = valid(usage.cost_usd);
  rows.push({ label: hasCost ? tr("Native cost estimate") : tr("Cost"), value: hasCost ? `USD ${fixed(usage.cost_usd as number, 4)}` : tr("Not reported") });
  const extras = object(usage.extras);
  for (const [key, label] of [["api_ms", trNoop("API time")], ["tool_ms", trNoop("Tool time")]] as const) {
    if (valid(extras[key])) rows.push({ label: tr(label), value: tr("%1 s", fixed((extras[key] as number) / 1000, 1)) });
  }
  for (const [key, label] of [["lines_added", trNoop("Lines added")], ["lines_removed", trNoop("Lines removed")]] as const) {
    if (valid(extras[key])) rows.push({ label: tr(label), value: integer(extras[key]) });
  }
  const costNote = !hasCost ? "" : usage.cost_incomplete === true ? tr("The provider reports incomplete cost data.")
    : usage.cost_stale === true ? tr("Last saved native cost snapshot. Newer requests are not included yet.")
    : tr("Provider estimate from its latest saved snapshot; may differ from the bill.");
  const context = object(usage.context);
  let contextText = "";
  if (valid(context.used)) {
    contextText = integer(context.used);
    if (valid(context.limit) && (context.limit as number) > 0) contextText += ` / ${integer(context.limit)} (${fixed(100 * (context.used as number) / (context.limit as number), 1)}%)`;
    if (context.estimated === true) contextText = tr("Approx. ") + contextText;
  }
  return {
    status: "", rows, costNote, context: contextText,
    footer: usage.partial === true ? tr("Reading session history. Totals are incomplete.")
      : tr("This conversation only. Subagent usage is separate. Cache is included in input; reasoning is included in output."),
  };
}
