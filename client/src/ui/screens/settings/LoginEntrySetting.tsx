import { Show, createSignal, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { Group, Status, ToggleRow } from "./ui";
import { createLoginEntry, loginEntryHint, type LoginEntry } from "./login-entry";
import { tr, trNoop } from "../../../i18n";
import { quitApp } from "../../../bridge";

export const LOGIN_ENTRY_LABEL = trNoop("Start Zerus at login");

/** WhiteWh fork: light mode at sign-in (platform/windows/light.rs). */
interface LightEntry { available: boolean; on: boolean }

/** Windows only: the native side reports it unavailable everywhere else. */
export default function LoginEntrySetting() {
  const entry = createLoginEntry({
    read: () => invoke<LoginEntry>("login_entry_view"),
    write: (enabled) => invoke<LoginEntry>("login_entry_set", { enabled }),
  });
  const [light, setLight] = createSignal<LightEntry>();
  const [lightBusy, setLightBusy] = createSignal(false);
  const [lightError, setLightError] = createSignal("");
  const readLight = async () => { try { setLight(await invoke<LightEntry>("light_entry_view")); } catch { setLight(undefined); } };
  const chooseLight = async (on: boolean) => {
    if (lightBusy()) return;
    setLightBusy(true);
    setLightError("");
    try {
      setLight(await invoke<LightEntry>("light_entry_set", { enabled: on }));
      // Light mode replaces the desktop's own sign-in entry.
      await entry.load();
    } catch (failure) {
      setLightError(String(failure));
      await readLight();
    } finally {
      setLightBusy(false);
    }
  };
  const switchNow = async () => {
    setLightError("");
    try {
      await invoke("light_mode_start");
      await quitApp();
    } catch (failure) {
      setLightError(String(failure));
    }
  };
  onMount(() => { void entry.load(); void readLight(); });
  return (
    <Show when={entry.available()}>
      <Group title={trNoop("Startup")}>
        <ToggleRow label={LOGIN_ENTRY_LABEL} hint={loginEntryHint(entry.state())} checked={entry.checked()} disabled={entry.disabled()} onChange={(on) => void entry.choose(on)} />
        <Show when={entry.error()}>{(text) => <Status kind="error">{text()}</Status>}</Show>
        <Show when={light()?.available}>
          <ToggleRow label={trNoop("Start in light mode at login")} checked={!!light()?.on} disabled={lightBusy()} onChange={(on) => void chooseLight(on)}
            hint={trNoop("Only the notification-area icon starts, with no window: it keeps this computer's session host and team worker running for another computer's Zerus, using almost no memory, processor or graphics. Open Zerus from its icon when you need the window. Replaces Start Zerus at login.")} />
          <div class="set-actions">
            <button class="btn small" onClick={() => void switchNow()}>{tr("Switch to light mode now")}</button>
          </div>
          <Show when={lightError()}>{(text) => <Status kind="error">{text()}</Status>}</Show>
        </Show>
      </Group>
    </Show>
  );
}
