import type { Inspect } from "../../types";
import { trNoop, tr } from "../../i18n";

// The buttons of a failed local message row (SessionsWindow.cpp messageAction,
// 4470-4527): Retry, Check Terminal or Check native UI, and Delete. Apart
// from the view so they can be tested.

/** Delete removes the row for good: it asks first (destructive actions confirm). */
export const DELETE_TITLE = trNoop("Delete message?");
export const DELETE_TEXT = trNoop("Remove this message from Activity? This cannot be undone.");
/** Retry while the composer is busy: Rust would refuse it (messageAction 4483). */
export const RETRY_AFTER_SEND = trNoop("Retry is available once the current message is sent.");
export const RETRY_WHEN_READY = trNoop("Retry is available once the composer is ready.");

/** Why Retry is off, "" when it is on: a send or a wait under way, or the composer locked. */
export function retryReason(view: { sending: boolean; waiting: string | null } | undefined, locked: boolean): string {
  if (view?.sending || view?.waiting) return tr(RETRY_AFTER_SEND);
  return locked ? RETRY_WHEN_READY : "";
}

export interface InspectPorts {
  /** The Terminal tab, connected (Qt's setCurrentWidget(m_terminal) and connectSession). */
  showTerminal(): void;
  showNativeUi(): void;
  notify(tone: "info", text: string): void;
}

/** Where Check leads (messageAction "inspect", 4477-4481); nothing once its composer went. */
export function routeInspect(inspect: Inspect | undefined, ports: InspectPorts): void {
  if (!inspect) return;
  if (inspect.kind === "terminal") ports.showTerminal();
  else if (inspect.kind === "native_ui") ports.showNativeUi();
  // Rust already reads the session again (Qt's inspect()); the notice says what to check.
  else ports.notify("info", inspect.notice);
}
