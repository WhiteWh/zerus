import { Show, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../../../bridge";
import { desktopService } from "../../../store/desktopService";
import { bus } from "../../../store/notices";
import { flag, setSetting } from "../../../store/settings";
import { motionReduced } from "../../motion";
import { Group, SectionHead, ToggleRow } from "./ui";
import { tr, trNoop } from "../../../i18n";
import { createNotificationPreferences } from "./notification-preferences";

// Settings > Sessions (tray/src/SettingsPage.h:97-107,
// QuestionNotificationSettings.h) as the approved mockup lays it out
// (design/webui/src/screens/settings/General.tsx), plus the client's Reduce
// motion: Qt reads workspace/reduceMotion but offers no control for it.

const toggle = (key: string) => (on: boolean) => void setSetting(key, on);

export function SessionsSection() {
  const notifications = createNotificationPreferences(desktopService.enabled, desktopService.state,
    () => flag("notifications/optionalQuestions"), enabled => setSetting("notifications/optionalQuestions", enabled),
    (command, args) => invoke(command, args), error => bus.notify("error", String(error), true));
  const notification = notifications.view;
  onMount(() => {if (inTauri() && desktopService.enabled()) void notifications.check();});
  const systemOnly = () => motionReduced() && !flag("workspace/reduceMotion");
  return (
    <>
      <SectionHead title={trNoop("Sessions")} />
      <Group>
        <ToggleRow label={trNoop("Expand the collapsed session list on hover")} checked={flag("workspace/expandSessionsOnHover")} onChange={toggle("workspace/expandSessionsOnHover")} />
        <ToggleRow label={trNoop("Hide projects without active sessions")} checked={flag("workspace/hideEmptyProjects")} onChange={toggle("workspace/hideEmptyProjects")} />
        <ToggleRow label={trNoop("Animate working sessions")} checked={flag("workspace/animateActivity")} onChange={toggle("workspace/animateActivity")}
          hint={motionReduced() ? tr("Reduce motion is on, so working sessions hold still.") : undefined} />
        <ToggleRow label={trNoop("Show completed subagents")} checked={flag("workspace/showCompletedSubagents")} onChange={toggle("workspace/showCompletedSubagents")}
          hint={trNoop("Include agents whose latest turn has finished in the session list and its counts. Plan & agents always shows all reported agents. Agents with unknown status remain visible.")} />
        <ToggleRow label={trNoop("Notify about optional questions")} checked={notifications.checked()} onChange={(on)=>void notifications.change(on)}
          disabled={notifications.disabled()}
          hint={trNoop("Off by default. Shows a system notification once for each new optional question, which the agent keeps working without. It is withdrawn when the questions are answered or skipped, or the session is marked as read. Questions that need an answer always notify.")} />
        <Show when={desktopService.enabled()}>
          <Show when={notification()?.pending} fallback={<Show when={!notification()?.available && !notification()?.code}><p class="set-group-hint">{tr("Notification settings are unavailable from the desktop service.")}</p></Show>}>
            <p class="set-group-hint">{tr("The previous notification setting is unconfirmed. Check its result before making another change.")}</p>
          </Show>
          <Show when={notification()?.code}><p class="set-group-hint">{tr(notification()!.code)}</p></Show>
          <button class="btn small" disabled={notifications.busy() || notification()?.running} onClick={()=>void notifications.check()}>{tr("Check result")}</button>
        </Show>
      </Group>
      <Group title={trNoop("Motion")}>
        <ToggleRow label={trNoop("Reduce motion")} checked={flag("workspace/reduceMotion")} onChange={toggle("workspace/reduceMotion")}
          hint={<>
            {tr("Stops endless motion: the Zerus mark, working sessions, loading indicators and the playing video note. Panels and menus appear without animating.")}
            <Show when={systemOnly()}><br />{tr("The system asks for reduced motion, so Zerus reduces it whatever this setting says.")}</Show>
          </>} />
      </Group>
      <p class="set-foot">{tr("Closing Zerus keeps agents running. Projects and the arrangement of your workspace are saved on this device.")}</p>
    </>
  );
}
