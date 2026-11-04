
import { trNoop, tr } from "../../../i18n";// Settings > Private analytics: the texts AnalyticsSettings.cpp writes from
// what the core's analytics page drew (zerus_client_core::analytics::page).
// Numbers QLocale writes follow the person's locale; those Qt writes with
// QString::arg stay plain.

export interface Histogram { counts: number[]; count: number; sum: number }
export interface Waits { observed: number; complete: boolean }
export interface ProviderLine { provider: string; partial: boolean | null; unsupported: string[] | null }
export interface ComputerReport {
  route: string; current: boolean; generated_at: number; revision: number; start: number | null; end: number | null;
  partial: boolean; gaps: string[]; unsupported: string[]; collecting: boolean; error: string; waits: Waits | null;
  providers: ProviderLine[];
}
export interface SpeechTotals { counters: Record<string, number>; error_codes: Record<string, number> }
export interface VoiceReport {
  route: string; error: string;
  read: { current: boolean; enabled: boolean; totals: SpeechTotals; identified: boolean } | null;
}
export interface Report {
  from: string; through: string; routes: number; present: number; totals: Record<string, number>;
  work: Histogram | null; peer_limit: boolean; missing: { route: string; error: string }[];
  computers: ComputerReport[]; voice: VoiceReport[]; buckets: number;
}
export interface Row {
  date: string; route: string; provider: string; producer: string; current: boolean;
  counters: Record<string, number>; histograms: Record<string, Histogram>;
}

export const PROVIDERS = ["codex", "claude", "kimi", "dsh", "opencode", "other"];
export const COUNTERS = [
  "turn_started", "turn_completed", "turn_interrupted", "work_ms", "approval_wait_ms", "input_wait_ms",
  "approval_requested", "approval_resolved", "input_requested", "input_resolved", "delivery_submitted",
  "delivery_failed", "delivery_uncertain", "recovery_attempt", "recovery_succeeded", "recovery_exhausted",
  "failure_authentication", "failure_rate_limit", "failure_quota", "failure_policy", "failure_network",
  "failure_service", "failure_unknown", "input_tokens", "output_tokens", "cache_tokens", "reasoning_tokens",
];
export const SPEECH_COUNTERS = ["submitted", "completed", "failed", "cancelled", "uncertain", "audio_ms", "queue_ms", "processing_ms"];
export const THIS_COMPUTER = trNoop("This computer");

/** QLocale().toString(n, 'f', decimals), or of a whole number. */
export function localNumber(n: number, decimals = 0, locale?: string): string {
  return new Intl.NumberFormat(locale, { minimumFractionDigits: decimals, maximumFractionDigits: decimals }).format(n);
}

export function displayRoute(route: string): string {
  return route === "" ? tr(THIS_COMPUTER) : route;
}

/** A reported measure: unavailable when absent, minutes for durations. */
export function measure(values: Record<string, number>, key: string, locale?: string): string {
  if (!(key in values)) return tr("Unavailable");
  const n = values[key];
  if (key.endsWith("_ms")) return tr("%1 min", localNumber(n / 60000, 2, locale));
  return localNumber(Math.trunc(n), 0, locale);
}

const pad = (n: number, width = 2) => String(n).padStart(width, "0");

/** Seconds since the epoch as `yyyy-MM-dd HH:mm:ss UTC`. */
export function timeText(seconds: number | null): string {
  if (seconds === null) return "Unknown";
  const t = new Date(Math.trunc(seconds * 1000));
  return `${pad(t.getUTCFullYear(), 4)}-${pad(t.getUTCMonth() + 1)}-${pad(t.getUTCDate())} ${pad(t.getUTCHours())}:${pad(t.getUTCMinutes())}:${pad(t.getUTCSeconds())} UTC`;
}

export function codes(values: string[]): string {
  return values.length ? values.map((code) => code.replaceAll("_", " ")).join(", ") : tr("None reported");
}

/** Each counter's line: its name in words and its measure. */
export function describeCounters(counts: Record<string, number>, names: string[], locale?: string): string {
  return names.map((name) => {
    let label = name.endsWith("_ms") ? name.slice(0, -3) : name;
    label = label.replaceAll("_", " ");
    if (name === "audio_ms") label = tr("Accepted audio");
    else if (name === "queue_ms") label = tr("Total queue time");
    else if (name === "processing_ms") label = tr("Total processing time");
    return `${label}: ${measure(counts, name, locale)}`;
  }).join("\n");
}

/** The overview above the coverage. */
export function overviewText(report: Report, locale?: string): string {
  let histogram = tr("Recorded fleet work histogram: Unavailable (missing, incompatible or overflowing samples).");
  if (report.work) {
    const count = report.work.count;
    const mean = count > 0 ? `${localNumber(report.work.sum / count, 1, locale)} ms` : tr("Unavailable (no samples)");
    histogram = tr("Recorded fleet work histogram: %1 samples; weighted mean %2.", localNumber(count, 0, locale), mean);
  }
  return tr("%1 through %2 UTC (%3 selected route(s), %4 current unique computer(s)). ", report.from, report.through, report.routes, report.present) +
    tr("Recorded current totals: %1 completed turns; %2 work.\n", measure(report.totals, "turn_completed", locale), measure(report.totals, "work_ms", locale)) +
    `${histogram}\n` + tr("Coverage below applies to every total and recorded sample.");
}

/** One computer's coverage: its paragraph, then its error, waits and providers. */
export function coverageText(c: ComputerReport): string {
  const lines = [
    `${displayRoute(c.route)} (${c.current ? "Current" : tr("Last known (excluded)")}); generated ${timeText(c.generated_at)}; revision ${Math.trunc(c.revision)}\n` +
    tr("Coverage %1 to %2; %3\n", timeText(c.start), timeText(c.end), c.partial ? "partial" : "no partial flag reported") +
    `Gaps: ${codes(c.gaps)}\nUnsupported: ${codes(c.unsupported)}\nCollection: ${c.collecting ? "enabled" : "disabled"}`,
  ];
  if (c.error) lines.push(c.error);
  if (c.waits) lines.push(tr("Current unresolved waits observed: %1 (%2)", Math.trunc(c.waits.observed), c.waits.complete ? "reported complete" : "incomplete observation"));
  for (const p of c.providers) {
    const known = p.partial !== null;
    lines.push(`${p.provider} coverage: ${known ? (p.partial ? "partial" : "reported") : "unknown"}; unsupported: ${known ? codes(p.unsupported ?? []) : "unknown"}`);
  }
  return lines.join("\n");
}

/** QJsonDocument's compact writing of an object of counts: keys in order. */
function compact(values: Record<string, number>): string {
  return `{${Object.keys(values).sort().map((key) => `${JSON.stringify(key)}:${values[key]}`).join(",")}}`;
}

export function voiceText(v: VoiceReport, locale?: string): string {
  if (!v.read) return `${displayRoute(v.route)}: ${v.error}`;
  const r = v.read;
  let text = `${displayRoute(v.route)} (${r.current ? "latest read" : "last known"}; speech collection ${r.enabled ? "enabled" : "disabled"})\n` +
    `${describeCounters(r.totals.counters, SPEECH_COUNTERS, locale)}\nTyped errors: ${compact(r.totals.error_codes)}`;
  if (!r.identified) text += "\n\nComputer identity unavailable; this route is not included in any fleet total.";
  return text;
}

/** A selected day: every reported count, then each sampled histogram's mean. */
export function detailText(row: Row, locale?: string): string {
  let text = describeCounters(row.counters, COUNTERS, locale);
  for (const name of Object.keys(row.histograms).sort()) {
    const h = row.histograms[name];
    if (h.count > 0) text += `\n${name} histogram mean: ${(h.sum / h.count).toFixed(1)} ms (${h.count} samples)`;
  }
  return text;
}

export function pageText(offset: number, shown: number, total: number): string {
  return tr("%1–%2 of %3 daily buckets (up to 50 per page)", shown ? offset + 1 : 0, offset + shown, total);
}

/** The default range: the last 30 UTC days through today. */
export function defaultRange(nowMs: number): { from: string; through: string } {
  const day = (ms: number) => new Date(ms).toISOString().slice(0, 10);
  return { from: day(nowMs - 29 * 86_400_000), through: day(nowMs) };
}

