import { uiLanguage, type UiLanguage } from "../../i18n";

// Shared day and time texts for usage, archive and transcript dates. English
// keeps Qt's C-locale short months ("3 Nov"); Russian takes the locale's full
// genitive month ("3 ноября"), assembled from its parts so no abbreviation or
// "г." dot reads as a separator. Clocks are 24 h in both.

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const two = (n: number) => String(n).padStart(2, "0");
let genitive: Intl.DateTimeFormat | undefined;
let weekday: Intl.DateTimeFormat | undefined;

/** The month as it follows a day: "Nov", or the Russian genitive "ноября". */
export function monthName(d: Date, language: UiLanguage = uiLanguage()): string {
  if (language !== "ru") return MONTHS[d.getMonth()];
  genitive ??= new Intl.DateTimeFormat("ru", { day: "numeric", month: "long" });
  return genitive.formatToParts(d).find((part) => part.type === "month")?.value ?? MONTHS[d.getMonth()];
}

/** "3 Nov" or "3 ноября", with the year after it when asked. */
export function dayMonth(d: Date, year = false, language: UiLanguage = uiLanguage()): string {
  return `${d.getDate()} ${monthName(d, language)}${year ? ` ${d.getFullYear()}` : ""}`;
}

/** "HH:mm", or "HH:mm:ss", in local time. */
export const clockOf = (d: Date, seconds = false): string =>
  `${two(d.getHours())}:${two(d.getMinutes())}${seconds ? `:${two(d.getSeconds())}` : ""}`;

/** Qt's "d MMM HH:mm" in local time ("3 ноября 02:27" in Russian); optionally with seconds. */
export const dayTimeOf = (d: Date, seconds = false, language: UiLanguage = uiLanguage()): string =>
  `${dayMonth(d, false, language)} ${clockOf(d, seconds)}`;

/** The transcript's message time: "Oct 27 09:12" as the design shows it, "27 октября 09:12" in Russian. */
export const messageTimeOf = (d: Date, language: UiLanguage = uiLanguage()): string =>
  language === "ru" ? dayTimeOf(d, false, language) : `${MONTHS[d.getMonth()]} ${d.getDate()} ${clockOf(d)}`;

/** Qt's "ddd, d MMM HH:mm:ss" for a last recorded event: "Tue, 27 Oct 11:43:08", "вт, 27 октября 11:43:08". */
export function eventTimeOf(d: Date, language: UiLanguage = uiLanguage()): string {
  const day = language === "ru" ? (weekday ??= new Intl.DateTimeFormat("ru", { weekday: "short" })).format(d) : DAYS[d.getDay()];
  return `${day}, ${dayTimeOf(d, true, language)}`;
}

/** A question's "Asked" time: the day and month, with the year when it is not this year, and seconds. */
export function askedTimeOf(d: Date, now: Date = new Date(), language: UiLanguage = uiLanguage()): string {
  return `${dayMonth(d, d.getFullYear() !== now.getFullYear(), language)} ${clockOf(d, true)}`;
}

/** The full local time with its UTC offset, as Qt's tooltip shows it: "28 Oct 2026 14:03:05 +03:00". */
export function zonedTimeOf(d: Date, language: UiLanguage = uiLanguage()): string {
  const minutes = -d.getTimezoneOffset(), sign = minutes < 0 ? "-" : "+", abs = Math.abs(minutes);
  return `${dayMonth(d, true, language)} ${clockOf(d, true)} ${sign}${two(Math.floor(abs / 60))}:${two(abs % 60)}`;
}
