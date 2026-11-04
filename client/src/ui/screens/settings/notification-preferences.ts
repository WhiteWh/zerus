import { createSignal } from "solid-js";
import type { DesktopService } from "../../../types";

type Command = "desktop_notification_preferences_set" | "desktop_notification_preferences_check";
/** The native projection owns pending intent identity. Invoke results never
 * replace it: an older IPC return can arrive after a newer watch batch. */
export function createNotificationPreferences(
  service: () => boolean,
  current: () => DesktopService | undefined,
  local: () => boolean,
  setLocal: (enabled: boolean) => Promise<unknown>,
  send: (command: Command, args?: { enabled: boolean }) => Promise<unknown>,
  report: (error: unknown) => void,
) {
  const [busy, setBusy] = createSignal(false);
  const view = () => current()?.notification_preferences;
  const disabled = () => busy() || (service() && (!current()?.connected || !view()?.available || !!view()?.pending || !!view()?.running));
  const change = async (enabled: boolean) => {
    if (disabled()) return;
    setBusy(true);
    try {
      if (service()) await send("desktop_notification_preferences_set", { enabled });
      else await setLocal(enabled);
    } catch (error) { report(error); }
    finally { setBusy(false); }
  };
  const check = async () => {
    if (!service() || busy() || view()?.running) return;
    setBusy(true);
    try { await send("desktop_notification_preferences_check"); }
    catch (error) { report(error); }
    finally { setBusy(false); }
  };
  return { busy, view, disabled, change, check,
    checked: () => service() ? view()?.status?.preferences.optional_questions === true : local() };
}
