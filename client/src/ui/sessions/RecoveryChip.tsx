import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Icon } from "../lib/icons";
import { ChipPopover } from "./ChipPopover";
import { useNow } from "../../store/latest";
import { computerOffline } from "../../store/catalog";
import { followedFor, notify } from "../../store/actions";
import { recoveryError, recoveryPending, runRecoveryAction } from "../../store/recovery";
import { refreshSessionUsage, sessionUsage } from "../../store/sessionUsage";
import { composer } from "../../store/composer";
import { setDetailTab } from "../../store/ui";
import { terminal } from "../../store/terminal";
import { recoveryInspection, recoveryUsageNotice, recoveryView } from "./recovery-ui";
import { nativeRoute } from "./native-entry";
import type { Session } from "./types";
import { tr } from "../../i18n";

/** RecoveryUi::Panel and its composer chip. */
export function RecoveryChip(props: { session: Session; compact: boolean }) {
  const now = useNow(1000);
  const [open, setOpen] = createSignal(false);
  const [keyboard, setKeyboard] = createSignal(false);
  const [historyOpen, setHistoryOpen] = createSignal(false);
  let chip!: HTMLButtonElement;
  let historyDialog: HTMLDialogElement | undefined;
  const online = () => !computerOffline(props.session.computer);
  const details = () => recoveryInspection(props.session, followedFor(props.session.key));
  const jobId = () => {
    const job = details().recovery;
    return job && typeof job === "object" && typeof (job as Record<string, unknown>).id === "string" ? (job as Record<string, unknown>).id as string : "";
  };
  const notice = () => recoveryUsageNotice(props.session, details(), sessionUsage(), online(), now());
  const view = () => recoveryView(details(), online(), now(), recoveryPending(props.session, jobId()), recoveryError(props.session, jobId()), notice());
  createEffect(() => { if (!view().active) { setOpen(false); setHistoryOpen(false); } });
  createEffect(() => {
    if (historyOpen() && historyDialog && !historyDialog.open) historyDialog.showModal();
    if (!historyOpen() && historyDialog?.open) historyDialog.close();
  });
  onCleanup(() => { if (historyDialog?.open) historyDialog.close(); });
  const historyText = () => view().attempts.map((entry) => `${tr("%1  %2 (%3 attempts)", entry.at, entry.label, entry.attempts)}${entry.reason ? `\n${entry.reason}` : ""}`).join("\n");
  const openTerminal = () => {
    setOpen(false);
    if (!online()) return;
    if (props.session.cmd === "dsh") { nativeRoute(props.session)(); return; }
    if (!terminal.texts()?.supported) { notify("error", terminal.texts()?.reason || tr("Terminal is unavailable for this session."), true); return; }
    terminal.opened();
    setDetailTab("terminal");
  };
  const openRecoverySettings = () => {
    setOpen(false);
    void import("../screens/settings/sections").then(({ openSettings }) => openSettings("recovery"));
  };
  return (
    <Show when={view().active}>
      <span class="att-anchor">
        <button ref={chip} type="button" class={`tchip ${view().tone === "danger" ? "danger" : "warn"}`} classList={{ on: open() }} aria-haspopup="dialog" aria-expanded={open()}
          aria-label={view().label} title={`${view().status}\n${view().detail}`.trim()}
          onClick={(event) => { setKeyboard(event.detail === 0); setOpen(!open()); }}>
          <Icon name="rotate-ccw" size={13} />
          {props.compact ? view().shortLabel : view().label}
        </button>
        <ChipPopover open={open()} anchor={chip} label={tr("Automatic recovery")} keyboard={keyboard()} class="recovery-pop" width={560} onClose={(focus) => { setOpen(false); if (focus) chip.focus(); }}>
          <p class="recovery-status">{view().status}</p>
          <Show when={view().detail}><p class="recovery-detail" title={view().detailFull}>{view().detail}</p></Show>
          <Show when={view().help}><p class="recovery-help">{view().help}</p></Show>
          <div class="recovery-actions">
            <Show when={view().now.visible}><button type="button" class="btn small" disabled={!view().now.enabled} onClick={() => void runRecoveryAction(props.session, view(), "now")}>{tr("Retry now")}</button></Show>
            <Show when={view().cancel.visible}><button type="button" class="btn small" disabled={!view().cancel.enabled} onClick={() => void runRecoveryAction(props.session, view(), "cancel")}>{tr("Cancel retry")}</button></Show>
            <Show when={view().history.visible}><button type="button" class="btn small" disabled={!view().history.enabled} onClick={() => { setOpen(false); setHistoryOpen(true); }}>{tr("Attempts…")}</button></Show>
            <Show when={view().terminal.visible}><button type="button" class="btn small" disabled={!view().terminal.enabled} onClick={openTerminal}>{tr("Open Terminal")}</button></Show>
            <Show when={view().usage.visible}><button type="button" class="btn small" disabled={!view().usage.enabled} onClick={() => { void refreshSessionUsage(true); void composer.refresh(); }}>{tr("Refresh usage")}</button></Show>
            <span class="grow" />
            <Show when={view().settingsVisible}><button type="button" class="btn small" onClick={openRecoverySettings}>{tr("Settings…")}</button></Show>
          </div>
        </ChipPopover>
      </span>
      <dialog ref={historyDialog} class="dlg recovery-history-dialog" aria-labelledby="recovery-history-title" onCancel={(event) => { event.preventDefault(); setHistoryOpen(false); }} onClose={() => setHistoryOpen(false)}>
        <div class="dlg-head"><h2 id="recovery-history-title">{tr("Recovery attempts — Zerus")}</h2></div>
        <div class="dlg-body"><textarea class="recovery-history" readonly aria-label={tr("Recovery attempts")} value={historyText()} /></div>
        <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn" data-initial onClick={() => setHistoryOpen(false)}>{tr("Close")}</button></div>
      </dialog>
    </Show>
  );
}
