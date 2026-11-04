// Account limit windows and their pace, ported from tray/src/AccountUsage.h.
// Pace compares used quota with the share of the window that has passed:
// spending faster than that share exhausts the limit before it resets.
// The core projects resets as Unix seconds; `fromProjected` turns them into
// minutes from `now` on each 15 s render, so nothing else runs on a timer.
import type { ProjectedWindow, Usage } from "../../types";
import { tr } from "../../i18n";
import { clockOf, dayTimeOf } from "./dates";

export interface UsageWindow {
  /** Window length in minutes (300 = 5 h, 10080 = 7 d); null when the provider reports none. */
  minutes: number | null;
  /** Used share, 0..100. */
  used: number;
  /** Minutes until reset; null when unknown (it counts as current, as in Qt); not positive once ended. */
  resetsIn: number | null;
  /** The provider's name for the window ("Sonnet", "Monthly"). */
  label?: string;
}

export type Tone = "normal" | "warning" | "critical";

export interface Pace {
  /** The window length and a future reset are known. */
  known: boolean;
  /** The first 8% of a window is too short to extrapolate. */
  early: boolean;
  used: number;
  /** Share of the window that has passed, 0..1. */
  elapsed: number;
  minutesLeft: number;
  /** Minutes until 100% at the average pace; -1 when it lasts until reset. */
  runsOutIn: number;
  /** Percent at reset at the average pace; -1 when unknown. */
  projected: number;
  tone: Tone;
}

/** A projected window at `now` (Unix seconds). */
export const fromProjected = (w: ProjectedWindow, now: number): UsageWindow => ({
  minutes: w.minutes, used: w.used, label: w.label, resetsIn: w.resets_at === null ? null : (w.resets_at - now) / 60,
});

export const windowsAt = (windows: readonly ProjectedWindow[], now: number) => windows.map((w) => fromProjected(w, now));

export const thresholdTone = (used: number): Tone => (used >= 90 ? "critical" : used >= 70 ? "warning" : "normal");

/** AccountUsage::current: a reset that is unknown or still ahead. */
export const current = (w: UsageWindow) => w.resetsIn === null || w.resetsIn > 0;

export function pace(w: UsageWindow): Pace {
  const used = Math.max(0, Math.min(100, w.used));
  const result: Pace = { known: false, early: false, used, elapsed: 0, minutesLeft: -1, runsOutIn: -1, projected: -1, tone: thresholdTone(used) };
  if (!w.minutes || w.minutes <= 0 || w.resetsIn === null || w.resetsIn <= 0) return result;
  result.known = true;
  result.minutesLeft = w.resetsIn;
  const passed = Math.max(0, w.minutes - w.resetsIn);
  result.elapsed = passed / w.minutes;
  result.early = result.elapsed < 0.08;
  // Exhausted and fresh windows keep the fixed 70/90% thresholds.
  if (used >= 100 || result.early) return result;
  result.projected = used / result.elapsed;
  if (result.projected > 100) result.runsOutIn = ((100 - used) * passed) / used;
  result.tone = result.runsOutIn < 0 ? "normal" : result.runsOutIn >= result.minutesLeft / 2 ? "warning" : "critical";
  if (used >= 90 && result.tone === "normal") result.tone = "warning";
  return result;
}

/** AccountUsage::period: the duration after the provider's name; "Usage" when neither is known. */
export function period(w: UsageWindow) {
  const m = w.minutes ?? 0;
  const duration = m === 10080 ? "7d" : m && m % 1440 === 0 ? `${m / 1440}d` : m && m % 60 === 0 ? `${m / 60}h` : m ? `${m}m` : "";
  const name = w.label ?? "";
  return name ? (duration ? `${name} ${duration}` : name) : duration || tr("Usage");
}

export function remaining(minutes: number | null) {
  if (minutes === null) return tr("reset unknown");
  const m = Math.max(0, Math.ceil(minutes));
  if (!m) return tr("refresh needed");
  if (m >= 1440) return tr("%1d %2h", Math.floor(m / 1440), Math.floor((m % 1440) / 60));
  if (m >= 60) return tr("%1h %2m", Math.floor(m / 60), m % 60);
  return tr("%1m", m);
}

/** Footer countdown: only the largest whole unit fits next to the percentage. */
export function countdown(minutes: number) {
  const m = Math.max(1, Math.ceil(minutes));
  return m < 60 ? tr("%1m", m) : m < 2880 ? tr("%1h", Math.floor(m / 60)) : tr("%1d", Math.floor(m / 1440));
}

/** "5h 34%", with the time to reset once usage reaches `resetFrom` percent. */
export function windowText(w: UsageWindow, withReset = true, resetFrom = 100) {
  if (!current(w)) return period(w) + tr(" refresh");
  return `${period(w)} ${Math.round(w.used)}%${withReset && w.used >= resetFrom ? ` (${remaining(w.resetsIn)})` : ""}`;
}

export function approximately(minutes: number) {
  const m = Math.max(1, Math.floor(minutes)), d = Math.floor(m / 1440), h = Math.floor((m % 1440) / 60), r = m % 60;
  if (d) return h ? tr("%1d %2h", d, h) : tr("%1d", d);
  if (h) return r ? tr("%1h %2m", h, r) : tr("%1h", h);
  return tr("%1m", m);
}

export function forecast(w: UsageWindow) {
  if (!current(w)) return "";
  const p = pace(w);
  if (!p.known || p.early || p.used >= 100) return "";
  return p.runsOutIn >= 0 ? tr("At the current pace, the limit runs out in about %1", approximately(p.runsOutIn))
    : tr("At the current pace, about %1% by reset", Math.round(p.projected));
}

const TONES: Tone[] = ["normal", "warning", "critical"];

/** AccountUsage::severest: the most urgent pace tone of the current windows; undefined when none reports usage. */
export function severest(windows: readonly UsageWindow[]): Tone | undefined {
  let tone: Tone | undefined;
  for (const w of windows) {
    if (!current(w)) continue;
    const next = pace(w).tone;
    if (!tone || TONES.indexOf(next) > TONES.indexOf(tone)) tone = next;
  }
  return tone;
}

const PROVIDERS: Record<string, string> = { claude: "Claude", codex: "Codex", kimi: "Kimi", opencode: "OpenCode", dsh: "DeepSeek", deepseek: "DeepSeek", sh: "Shell" };
/** IdentityBadges::providerName. */
export const providerName = (provider: string) => PROVIDERS[provider] ?? provider;

/** No window to draw yet: the meters show their placeholders. */
export const usageLoading = (u: Usage) => u.status === "loading" && !u.windows.length;

/** What replaces the meters without windows (AccountUsage::Button::summaryText). */
export function summaryText(u: Usage) {
  const b = u.balances[0];
  if (b) return `${b.balance} ${b.currency}`;
  if (u.status === "configured") return u.provider === "opencode" ? tr("Configured") : tr("API key");
  if (u.status === "signed_out") return tr("Sign in required");
  if (u.status === "expired") return tr("Sign-in expired");
  if (u.status === "unavailable" || u.refresh_error || u.offline) return tr("limits n/a");
  return tr("Unavailable");
}

/** AccountUsage::state: what the usage panel says without windows or balances. */
export function stateText(u: Usage) {
  const s = u.status;
  if (!s || s === "loading") return tr("Loading account details…");
  if (s === "configured" && u.provider === "opencode") return tr("OpenCode uses its native provider configuration. Usage is not available here.");
  if (s === "configured") return tr("API key configured. Usage is available in the DeepSeek console.");
  if (u.offline || s === "offline") return tr("Machine is offline. Showing the last available account data.");
  if (s === "expired") return tr("Native sign-in has expired. Open the agent or sign in again, then refresh usage.");
  if (s === "signed_out" && u.provider === "dsh") return tr("DeepSeek account is signed out. API-key connections are configured separately in the native settings.");
  if (s === "signed_out") return tr("No native subscription sign-in found. API-key sessions may still work.");
  if (s === "credentials_locked") return tr("Saved sign-in is in the locked macOS Keychain. Unlock it in Keychain Access, then refresh.");
  if (s === "desktop_session_unavailable") return tr("Saved sign-in requires the Mac desktop session. Log in to macOS and check the HGS installation, then refresh.");
  if (s === "credentials_unavailable") return tr("The saved Keychain sign-in could not be used. Check account access or sign in again.");
  return tr("Usage is unavailable from this provider. Open its native account page or refresh after signing in.");
}

/** Qt's "d MMM HH:mm" in local time ("3 ноября 02:27" in Russian); optionally include seconds. */
export const dayTime = (seconds: number, withSeconds = false) => dayTimeOf(new Date(seconds * 1000), withSeconds);

/** Qt's "HH:mm" in local time. */
export const clock = (seconds: number) => clockOf(new Date(seconds * 1000));

/** AccountUsage::tooltip (AccountUsage.h:197-219). */
export function accountTooltip(u: Usage, now: number) {
  const lines: string[] = [];
  const provider = providerName(u.provider);
  if (provider) lines.push(provider);
  if (u.label && u.label !== provider) lines.push(u.label);
  for (const value of [u.identity.name, u.identity.email, u.identity.plan, u.identity.organization]) if (value) lines.push(value);
  for (const p of u.windows) {
    const w = fromProjected(p, now);
    lines.push(tr("%1: %2% used", period(w), Math.round(w.used)));
    if (p.resets_at !== null) lines.push(tr("Resets %1", dayTime(p.resets_at)) + (current(w) ? tr(" (in %1)", remaining(w.resetsIn)) : ""));
    const f = forecast(w);
    if (f) lines.push(f);
  }
  for (const b of u.balances) lines.push(`${b.kind}: ${b.balance} ${b.currency}`);
  if (u.checked_at !== null) {
    if (u.seeded) lines.unshift(tr("Checked %1", clock(u.checked_at)));
    else lines.push(tr("Updated %1", clock(u.checked_at)));
  }
  if (u.refreshing) lines.push(tr("Updating… Previous values remain visible."));
  else if (u.refresh_error) lines.push(tr("Update failed. Showing the last available values."));
  if (u.offline) lines.push(tr("Machine is offline."));
  if (!u.windows.length && !u.balances.length) lines.push(stateText(u));
  lines.push(tr("Updates every 5 minutes. Open account details."));
  return lines.join("\n");
}

/** AccountUsage::limitRecoveryNotice: a fresh below-limit snapshot may explain
 * an early reset, but never acknowledges the failed native turn. */
export function limitRecoveryNotice(u: Usage, failureAt: number, now: number) {
  const checked = u.checked_at ?? 0;
  if (failureAt <= 0 || u.status !== "ok" || u.offline || u.refresh_error || checked <= failureAt || checked > now + 5 || now - checked > 300) return "";
  let session = false, week = false;
  for (const p of u.windows) {
    const w = fromProjected(p, now);
    if (!Number.isFinite(w.used) || w.used < 0 || w.used >= 100 || p.resets_at === null || p.resets_at <= now) return "";
    session ||= p.minutes === 300;
    week ||= p.minutes === 10080;
  }
  return session && week
    ? tr("Latest account usage is below the limit. Answer any open usage-limit choice in Activity or Terminal, then send a message to continue.")
    : "";
}

/** AccountUsage::exhausted, including the cached/offline explanation. */
export function exhausted(u: Usage, now: number) {
  const lines: string[] = [];
  for (const p of u.windows) {
    if (p.used < 100) continue;
    const w = fromProjected(p, now);
    lines.push(!current(w) ? tr("%1 limit window ended; refresh usage", period(w))
      : p.resets_at !== null ? tr("%1 limit reached. Resets in %2", period(w), remaining(w.resetsIn))
      : tr("%1 limit reached. Reset time is unavailable", period(w)));
  }
  const text = lines.join("\n");
  return text && (u.offline || u.refresh_error) ? tr("Last reported usage: %1. Refresh to check current limits.", text) : text;
}

export interface ExhaustedSummary { label: string; shortLabel: string; remaining: string }

/** The Usage limit chip. Its reset is a separate value, never a dot-separated suffix. */
export function exhaustedSummary(u: Usage, now: number): ExhaustedSummary | undefined {
  let nearest: number | undefined, reached = false, ended = false;
  for (const p of u.windows) {
    if (p.used < 100) continue;
    const w = fromProjected(p, now);
    if (!current(w)) { ended = true; continue; }
    reached = true;
    if (p.resets_at !== null && (nearest === undefined || p.resets_at < nearest)) nearest = p.resets_at;
  }
  if (reached) return { label: tr("Limit reached"), shortLabel: tr("Limit"), remaining: nearest === undefined ? "" : remaining((nearest - now) / 60) };
  return ended ? { label: tr("Refresh usage"), shortLabel: tr("Limit"), remaining: "" } : undefined;
}

/** The mockup's usage chip shows the highest current percentage from 50%. */
export function usageChipPercent(u: Usage, now: number): number | undefined {
  let highest: number | undefined;
  for (const p of u.windows) {
    const w = fromProjected(p, now);
    if (!current(w) || !Number.isFinite(w.used) || w.used < 50) continue;
    highest = Math.max(highest ?? 0, w.used);
  }
  return highest === undefined ? undefined : Math.round(highest);
}
