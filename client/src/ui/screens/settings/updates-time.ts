import { tr } from "../../../i18n";
import { clockOf, dayMonth } from "../../lib/dates";

/** The last successful check in the interface language: "Never" (the app's sentinel), or local day, month and time. */
export function lastCheckedText(raw: string, now: Date = new Date()): string {
  if (raw === "Never") return tr("Never");
  const at = new Date(raw);
  if (Number.isNaN(at.getTime())) return raw;
  return `${dayMonth(at, at.getFullYear() !== now.getFullYear())} ${clockOf(at)}`;
}
