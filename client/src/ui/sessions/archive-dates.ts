// The Archive's date range, after tray/src/ArchiveDatePicker.cpp: All dates,
// Today, Last 7 days, Last 30 days and a custom range, both dates included in
// local time. Presets and date edits change only the draft; Apply commits it.
// Days are local calendar days written YYYY-MM-DD (what <input type="date">
// reads and writes); they compare as text.
import { tr, trNoop } from "../../i18n";
import { dayMonth } from "../lib/dates";

/** All dates, Today, Last 7 days, Last 30 days, custom. */
export type Period = 0 | 1 | 2 | 3 | 4;

/** The applied range: the period and the custom range's days (kept while a preset is applied). */
export interface DateRange { period: Period; first: string; last: string }

/** The picker's draft; "" is Any date (All dates staged). */
export interface Draft { period: Period; from: string; until: string }

export const PERIODS = [trNoop("All dates"), trNoop("Today"), trNoop("Last 7 days"), trNoop("Last 30 days")] as const;
/** The upper bound of All dates (Qt's 253402300800, year 10000). */
export const ALL_UNTIL = 253402300800;
/** QDateEdit's limits in the picker. */
export const MIN_DAY = "1970-01-02";
export const MAX_DAY = "9999-12-30";

const pad = (n: number, width = 2) => String(n).padStart(width, "0");
const parts = (day: string) => day.split("-").map(Number) as [number, number, number];
const dayOf = (d: Date) => `${pad(d.getFullYear(), 4)}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;

/** The local calendar day of `now`. */
export const localDay = (now: Date) => dayOf(now);

export function addDays(day: string, n: number): string {
  const [y, m, d] = parts(day);
  return dayOf(new Date(y, m - 1, d + n));
}

/** Local midnight of a day in epoch seconds (QDate::startOfDay). */
export function startOfDay(day: string): number {
  const [y, m, d] = parts(day);
  return Math.floor(new Date(y, m - 1, d).getTime() / 1000);
}

/** The preset's first day: today, the last 7 days or the last 30 days. */
const presetFirst = (period: 1 | 2 | 3, today: string) => addDays(today, period === 1 ? 0 : period === 2 ? -6 : -29);

/** Qt's defaults: All dates, with the custom range at the last 30 days. */
export function initialRange(now: Date): DateRange {
  const today = localDay(now);
  return { period: 0, first: addDays(today, -29), last: today };
}

/** The archive window [from, until) in epoch seconds (ArchiveDatePicker::range). */
export function rangeBounds(r: DateRange, now: Date): [number, number] {
  if (r.period === 0) return [0, ALL_UNTIL];
  const today = localDay(now);
  const first = r.period === 4 ? r.first : presetFirst(r.period, today);
  const last = r.period === 4 ? r.last : today;
  return [startOfDay(first), startOfDay(addDays(last, 1))];
}

function formatDay(day: string, year: boolean): string {
  const [y, m, d] = parts(day);
  return dayMonth(new Date(y, m - 1, d), year);
}

/** The picker's caption (updateCaption): the preset, one day, or the two days with an en dash. */
export function rangeCaption(r: DateRange): string {
  if (r.period !== 4) return tr(PERIODS[r.period]);
  if (r.first === r.last) return formatDay(r.first, true);
  const sameYear = r.first.slice(0, 4) === r.last.slice(0, 4);
  return `${formatDay(r.first, !sameYear)} – ${formatDay(r.last, true)}`;
}

/** The picker button's accessible name and tooltip. */
export const pickerLabel = (r: DateRange) => tr("Archived date: %1. Choose period", rangeCaption(r));

/** The draft a period stages (stage): Any date for All dates, the preset's days, or the applied custom range. */
export function stageDraft(period: Period, applied: DateRange, now: Date): Draft {
  if (period === 0) return { period, from: "", until: "" };
  if (period === 4) return { period, from: applied.first, until: applied.last };
  const today = localDay(now);
  return { period, from: presetFirst(period, today), until: today };
}

/**
 * A date edit makes the draft a custom range (editDraft). Starting from All
 * dates supplies a finite companion date: the last 30 days.
 */
export function editDraft(d: Draft, edited: "from" | "until", value: string, now: Date): Draft {
  const today = localDay(now);
  const next: Draft = { ...d, period: 4, [edited]: value };
  if (d.period === 0) {
    if (edited !== "from") next.from = addDays(today, -29);
    if (edited !== "until") next.until = today;
  }
  return next;
}

/**
 * A day the picker's QDateEdit can hold: a four-digit year inside its range.
 * A typed date input can hold others (an empty field, year 0099, 1970-01-01);
 * QDateEdit cannot, so those only keep Apply off.
 */
export const dayInRange = (day: string) => /^\d{4}-\d{2}-\d{2}$/.test(day) && day >= MIN_DAY && day <= MAX_DAY;

export const draftValid = (d: Draft) => d.period === 0 || (dayInRange(d.from) && dayInRange(d.until) && d.from <= d.until);

/** Two days the edits can hold, the end before the start (Qt's invalid hint). */
export const draftReversed = (d: Draft) => d.period !== 0 && dayInRange(d.from) && dayInRange(d.until) && d.from > d.until;

/** Qt's hints; a day Qt's edits cannot hold keeps the neutral one (Apply stays off). */
export function draftHint(d: Draft): string {
  if (d.period === 0) return tr("All archived dates (no date limits)");
  return draftReversed(d) ? tr("End date must be on or after the start date.") : tr("Both dates included (local time)");
}

/** Apply: the range it commits and whether that changed it; undefined for an invalid custom range (Apply is off). */
export function applyDraft(d: Draft, applied: DateRange): { range: DateRange; changed: boolean } | undefined {
  if (!draftValid(d)) return undefined;
  const custom = d.period === 4;
  const changed = applied.period !== d.period || (custom && (applied.first !== d.from || applied.last !== d.until));
  return { range: custom ? { period: 4, first: d.from, last: d.until } : { ...applied, period: d.period }, changed };
}

/** The picker button's box in viewport coordinates (getBoundingClientRect). */
export interface ButtonBox { left: number; top: number; bottom: number }

/**
 * Where the panel goes (ArchiveDatePicker::openPanel), in viewport
 * coordinates for a fixed panel outside any transformed container: 480 px
 * wide at most, at the button's left, 6 px below it, or above it when it does
 * not fit, always 8 px inside the window.
 */
export function panelPlace(button: ButtonBox, height: number, viewport: { width: number; height: number }): { left: number; top: number; width: number } {
  const width = Math.min(480, viewport.width - 16);
  const left = Math.max(8, Math.min(button.left, viewport.width - width - 8));
  const below = button.bottom + 6;
  const top = below + height <= viewport.height - 8 ? below : button.top - height - 6;
  return { left, top: Math.max(8, Math.min(top, viewport.height - height - 8)), width };
}

/**
 * The control Tab (or Shift+Tab, `back`) moves to among the panel's `count`
 * controls from `index` (-1: the focus is outside the panel): it wraps, so the
 * focus stays in the panel as in Qt's popup. Undefined without controls.
 */
export function tabTarget(count: number, index: number, back: boolean): number | undefined {
  if (count <= 0) return undefined;
  if (index < 0) return back ? count - 1 : 0;
  return (index + (back ? -1 : 1) + count) % count;
}
