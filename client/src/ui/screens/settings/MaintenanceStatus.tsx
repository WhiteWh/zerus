import { Show, onMount } from "solid-js";
import { maintenanceIntent } from "../../../store/maintenance";
import { desktopService } from "../../../store/desktopService";
import { tr } from "../../../i18n";
import { Actions, Status } from "./ui";

/** Mounted only in service mode. Mounting reads a receipt, never configures. */
export function MaintenanceStatus(props: { apply?: boolean }) {
  onMount(() => { void maintenanceIntent.check().catch(() => {}); });
  return <>
    <Show when={maintenanceIntent.error()}>{(message) => <Status kind="error">{message()}</Status>}</Show>
    <Show when={maintenanceIntent.blocked()}>
      <Status kind="warn">{tr("The previous settings change is unconfirmed.")}</Status>
      <Actions><button type="button" class="btn" disabled={maintenanceIntent.busy()}
        onClick={() => void maintenanceIntent.check().catch(() => {})}>{tr("Check result")}</button></Actions>
    </Show>
    <Show when={props.apply && !maintenanceIntent.blocked()}>
      <p class="set-group-hint">{tr("Apply current update preferences and connect shared recovery settings. They continue in the desktop service when this window is closed.")}</p>
      <Actions><button type="button" class="btn" disabled={maintenanceIntent.busy() || !desktopService.ready("fleet")}
        onClick={() => void maintenanceIntent.submit().catch(() => {})}>{tr("Apply background settings")}</button></Actions>
    </Show>
    <Show when={props.apply && !maintenanceIntent.error() && !maintenanceIntent.blocked() && maintenanceIntent.result()?.state === "applied"}>
      <Status kind="ok">{tr("Settings applied.")}</Status>
    </Show>
  </>;
}
