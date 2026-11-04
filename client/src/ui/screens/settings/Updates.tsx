import { Show, createEffect, createSignal, onMount } from "solid-js";
import { refreshUpdates, updateCommands, updates } from "../../../store/updates";
import { toast } from "../../../store/ui";
import type { UpdateChannel } from "../../../types";
import { Icon } from "../../lib/icons";
import { Actions, Group, Lines, Row, SectionHead, Select, Status, ToggleRow } from "./ui";
import { lastCheckedText } from "./updates-time";
import { tr, trNoop } from "../../../i18n";
import { trText } from "../../../text";
import { desktopService } from "../../../store/desktopService";
import { maintenanceIntent } from "../../../store/maintenance";
import { MaintenanceStatus } from "./MaintenanceStatus";

function failed(error: unknown) {
  toast("Updates", String(error), "error", true);
}

function LoginItemDialog() {
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  let dialog!: HTMLDialogElement;
  createEffect(() => {
    if (open()) {
      if (!dialog.open) dialog.showModal();
      queueMicrotask(() => dialog.querySelector<HTMLElement>("[data-initial]")?.focus());
    } else if (dialog.open) dialog.close();
  });
  const fix = (restart: boolean) => {
    setBusy(true);
    void updateCommands.fixLoginItem(restart).then((restartError) => {
      setOpen(false);
      if (restartError) {
        toast(tr("Login item"), tr("The Zerus login item is updated; it applies at your next login."));
        toast(tr("Fix the Zerus login item"), tr("Zerus could not restart:\n%1", restartError), "error", true);
      } else if (restart) {
        toast(tr("Restarting Zerus"), tr("The login item is updated. Drafts are saved; agents keep running."));
      } else {
        toast(tr("Login item"), tr("The Zerus login item is updated; it applies at your next login."));
      }
    }).catch((error) => {
      toast(tr("Fix the Zerus login item"), tr("The login item was not changed.\n\n%1", String(error)), "error", true);
    }).finally(() => setBusy(false));
  };
  return (
    <>
      <Show when={updates().login_item_deprioritized}>
        <div class="notice warn upd-login">
          <Icon name="triangle-alert" size={14} />
          <span class="grow">{tr("macOS starts Zerus at background priority, so it can stop responding while the computer is busy.")}</span>
          <button type="button" class="btn small" onClick={() => setOpen(true)}>{tr("Fix login item")}</button>
        </div>
      </Show>
      <dialog id="login-item-dialog" class="dlg" ref={dialog} style={{ width: "520px" }} role="alertdialog" aria-labelledby="login-item-title" aria-describedby="login-item-text"
        onCancel={(event) => { event.preventDefault(); setOpen(false); }} onClose={() => setOpen(false)}>
        <div class="dlg-head"><Icon name="triangle-alert" size={18} class="warn" /><h2 id="login-item-title">{tr("Fix the Zerus login item")}</h2></div>
        <div class="dlg-body confirm-body" id="login-item-text">
          <p><b>{tr("macOS runs Zerus at background priority.")}</b></p>
          <p class="muted">{tr("Zerus was started by its login item, which macOS runs at background priority, so Zerus can stop responding while other programs keep the computer busy. Updating the login item makes macOS start Zerus like other apps.")}</p>
          <p class="muted">{tr("Update Only applies this at your next login; Zerus keeps running.")}<br />{tr("Update and Restart applies it now: Zerus quits and opens again. Saved drafts, sessions and agents stay; a voice or video message that is still recording stops and is not kept.")}</p>
        </div>
        <div class="dlg-foot">
          <button type="button" class="btn" disabled={busy()} onClick={() => setOpen(false)}>{tr("Cancel")}</button>
          <span class="spacer" />
          <button type="button" class="btn" disabled={busy()} onClick={() => fix(true)}>{tr("Update and Restart")}</button>
          <button type="button" class="btn primary" data-initial autofocus disabled={busy()} onClick={() => fix(false)}>{tr("Update Only")}</button>
        </div>
      </dialog>
    </>
  );
}

export function UpdatesSection() {
  const [details, setDetails] = createSignal(false);
  onMount(() => { void refreshUpdates(); });
  const open = (target: "release" | "download" | "instructions") => {
    void updateCommands.open(target).catch(failed);
  };
  const changing = () => desktopService.enabled() && (maintenanceIntent.busy() || maintenanceIntent.blocked());
  const change = (save: () => Promise<void>) => desktopService.enabled() ? maintenanceIntent.submit(undefined, save) : save();
  return (
    <>
      <SectionHead title={trNoop("Updates")} />
      <Group title={trText(updates().heading)}>
        <div class="updates-card" data-available={updates().available ? "1" : "0"}>
          <Row label={trNoop("Installed")}><span class="selectable">{trText(updates().installed)}</span></Row>
          <Row label={trNoop("Latest release")}><span class="selectable">{trText(updates().latest)}</span></Row>
          <Show when={!updates().busy && updates().comparison}>{(comparison) =>
            <p class="set-group-hint selectable updates-comparison">{trText(comparison())}</p>}
          </Show>
          <p class="set-group-hint selectable">{tr("Last successful check: %1", lastCheckedText(updates().last_checked))}</p>
          <Show when={updates().cached}><p class="set-group-hint">{tr("Showing the last verified catalog.")}</p></Show>
          <Show when={updates().error}>{(error) => <Status kind="error">{trText(error())}</Status>}</Show>
          <Actions>
            <button type="button" class="btn" disabled={!updates().release_enabled} onClick={() => open("release")}>{trText(updates().release_label)}</button>
            <Show when={updates().download_label}>
              {(label) => <button type="button" class="btn" onClick={() => open("download")}>{trText(label())}</button>}
            </Show>
          </Actions>
        </div>
      </Group>

      <LoginItemDialog />

      <Group title={trNoop("Update checks")}>
        <ToggleRow label={trNoop("Check for updates automatically")} checked={updates().automatic}
          disabled={changing()}
          hint={trNoop("Available updates appear in the tray and workspace, with a system notification.")}
          onChange={(on) => { void change(() => updateCommands.setAutomatic(on)).catch(failed); }} />
        <Row label={trNoop("Release channel")} for="updates-channel">
          <Select id="updates-channel" label={trNoop("Release channel")} value={updates().channel}
            disabled={changing()}
            options={[{ value: "stable", label: trNoop("Stable") }, { value: "nightly", label: trNoop("Nightly") }, { value: "dev", label: trNoop("Dev") }]}
            onChange={(channel) => { void change(() => updateCommands.setChannel(channel as UpdateChannel)).catch(failed); }} />
          <button type="button" class="btn" disabled={updates().busy} onClick={() => { void updateCommands.check().catch(failed); }}>{tr("Check now")}</button>
        </Row>
        <Show when={desktopService.enabled()}><MaintenanceStatus apply /></Show>
      </Group>

      <Group>
        <Actions>
          <button type="button" class="btn" aria-expanded={details()} aria-controls="updates-installation-details"
            onClick={() => setDetails(!details())}>{details() ? tr("Hide installation details") : tr("Show installation details")}</button>
        </Actions>
        <Show when={details()}>
          <div id="updates-installation-details" class="updates-details">
            <Lines class="selectable" text={trText(updates().details.ownership)} />
            <Show when={updates().details.command}>{(command) => <>
              <Lines class="selectable mono updates-command" text={trText(command())} />
              <Actions>
                <button type="button" class="btn" aria-label={updates().details.copy_name ? trText(updates().details.copy_name!) : undefined}
                  onClick={() => { void updateCommands.copyCommand().catch(failed); }}>{tr("Copy command")}</button>
              </Actions>
            </>}</Show>
            <Show when={updates().details.instructions}>{(instructions) =>
              <Lines class="selectable" text={trText(instructions())} />}
            </Show>
            <Actions>
              <button type="button" class="btn" onClick={() => open("instructions")}>{tr("Installation instructions")}</button>
            </Actions>
          </div>
        </Show>
      </Group>
    </>
  );
}
