import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { AgentBadge } from "./SessionCard";
import { Icon } from "../lib/icons";
import { ChipPopover } from "./ChipPopover";
import { UsageMeters, type UsageIndicator } from "../components/UsageMeter";
import { useNow } from "../../store/latest";
import { followSessionUsage, refreshSessionUsage, sessionUsage, startSessionUsage, stopSessionUsage } from "../../store/sessionUsage";
import { setAccountMachine, setRequestedProfile, loadAccounts } from "../../store/accounts";
import { setPage } from "../../store/ui";
import { openInspector } from "../../store/inspector";
import { text } from "../../store/settings";
import { composer } from "../../store/composer";
import { computerOffline } from "../../store/catalog";
import { followedFor } from "../../store/actions";
import { accountTooltip, exhausted, exhaustedSummary, usageChipPercent, usageLoading, windowsAt } from "../lib/usage";
import { recoveryInspection, recoveryUsageNotice } from "./recovery-ui";
import { footerUsageSummary } from "./session-usage-ui";
import type { Session } from "./types";
import { tr } from "../../i18n";

const toneClass = (tone: string) => tone === "danger" ? "danger" : tone === "warning" ? "warn" : tone === "success" ? "ok" : "";

/** AccountUsage::Button and RefreshButton in the bottom status bar. */
export function AccountUsageChip(props: { session: Session }) {
  const now = useNow(30_000);
  onMount(startSessionUsage);
  createEffect(() => {
    const s = props.session;
    followSessionUsage(s.tracked && !s.archiveId ? { host: s.computer, name: s.name, run_id: s.runId ?? "" } : null, s.cmd);
  });
  onCleanup(() => { followSessionUsage(null); stopSessionUsage(); });
  const u = sessionUsage;
  const online = () => !computerOffline(props.session.computer);
  const openAccount = () => {
    const s = props.session;
    setAccountMachine(s.computer);
    setRequestedProfile(u().id ? `${s.computer}\u001f${u().id}` : undefined);
    setPage("accounts");
    void loadAccounts();
  };
  return (
    <Show when={props.session.tracked && !props.session.archiveId}>
      <span class="session-usage-strip">
        <button type="button" class="sb-usage account-usage" title={accountTooltip(u(), now())} aria-label={`${tr("Account usage")}\n${exhausted(u(), now())}\n${accountTooltip(u(), now())}`} onClick={openAccount}>
          <Show when={u().provider}><AgentBadge agent={props.session.agent} /></Show>
          <UsageMeters windows={windowsAt(u().windows, now())} loading={usageLoading(u())} summary={footerUsageSummary(u())} indicator={text("workspace/usageIndicator") as UsageIndicator} />
        </button>
        <button type="button" class="icon-btn small usage-refresh" disabled={!online() || u().refreshing} aria-label={tr("Refresh usage")}
          title={u().refreshing ? tr("Updating account usage…") : tr("Refresh usage")} onClick={() => void refreshSessionUsage(true)}>
          <Show when={!u().refreshing} fallback={<span class="su-spin"><Icon name="refresh-cw" size={12} /></span>}><Icon name="refresh-cw" size={12} /></Show>
        </button>
      </span>
    </Show>
  );
}

/** The mockup's usage chip and the existing cached/offline warning popover. */
export function UsageLimitChip(props: { session: Session; compact: boolean }) {
  const now = useNow(30_000);
  const [open, setOpen] = createSignal(false);
  const [keyboard, setKeyboard] = createSignal(false);
  let chip!: HTMLButtonElement;
  const u = sessionUsage;
  const details = () => recoveryInspection(props.session, followedFor(props.session.key));
  const exhaustedText = () => exhausted(u(), now());
  const notice = () => recoveryUsageNotice(props.session, details(), u(), !computerOffline(props.session.computer), now());
  const warning = () => exhaustedText() || notice();
  const percent = () => usageChipPercent(u(), now());
  const summary = () => exhaustedSummary(u(), now());
  const label = () => percent() !== undefined ? tr("Usage %1%", percent()!) : summary()?.label ?? tr("Usage available");
  createEffect(() => { if (percent() === undefined && !warning()) setOpen(false); });
  return (
    <Show when={percent() !== undefined || warning()}>
      <span class="att-anchor">
        <button ref={chip} type="button" class={`tchip ${percent() !== undefined ? "warn" : exhaustedText() ? "danger" : ""}`} classList={{ on: open() }} aria-haspopup="dialog" aria-expanded={open()}
          aria-label={label()} title={warning() || label()}
          onClick={(event) => { setKeyboard(event.detail === 0); setOpen(!open()); }}>
          <Icon name={percent() !== undefined ? "gauge" : "attention"} size={13} />
          <Show when={percent() !== undefined} fallback={<Show when={props.compact} fallback={<>{(summary()?.label ?? tr("Usage available"))}<Show when={summary()?.remaining}><span class="usage-reset">{summary()!.remaining}</span></Show></>}>
            {summary()?.shortLabel ?? tr("Usage")}
          </Show>}>
            {props.compact ? `${percent()}%` : label()}
          </Show>
        </button>
        <ChipPopover open={open()} anchor={chip} label={tr("Account usage limit")} keyboard={keyboard()} class="usage-limit-pop" onClose={(focus) => { setOpen(false); if (focus) chip.focus(); }}>
          <p class="usage-warning">{warning() || label()}</p>
          <div class="cache-actions"><button type="button" class="btn small" disabled={computerOffline(props.session.computer) || u().refreshing} onClick={() => void refreshSessionUsage(true)}>{tr("Refresh usage")}</button></div>
        </ChipPopover>
      </span>
    </Show>
  );
}

/** Context opens the mockup's Details tab and its existing Session usage card. */
export function SessionUsageChip() {
  const context = () => composer.view()?.context;
  return (
    <Show when={context()?.visible}>
      <button type="button" class={`tchip ${toneClass(context()!.tone)}`} title={context()!.tooltip} aria-label={context()!.accessible_name}
        onClick={() => openInspector("details")}>
        <Show when={!context()!.warning} fallback={<Icon name="context-warning" size={13} />}>
          <span class="ctx-ring" style={{ "--p": `${Math.max(0, Math.min(100, context()!.percent ?? 0))}%` }} />
        </Show>
        {context()!.text}
      </button>
    </Show>
  );
}
