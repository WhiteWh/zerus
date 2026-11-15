import { For, Show, createMemo } from "solid-js";
import { composer } from "../../store/composer";
import { computerOffline } from "../../store/catalog";
import { notify } from "../../store/actions";
import { terminal } from "../../store/terminal";
import { selected, setDetailTab, subagentOpen } from "../../store/ui";
import { tr } from "../../i18n";
import { trText } from "../../text";
import { Icon } from "../lib/icons";
import { providerName } from "../lib/usage";
import { heldInputs, heldMessage, simplified, terminalDraftBar } from "./held-message";
import { nativeRoute } from "./native-entry";

/**
 * A "Compact and continue" message while the agent compacts first
 * (zerus-pabah): shown as pending above the composer, so it is never out of
 * sight, until it is sent. If it is not sent, it goes back to the composer.
 */
export function HeldMessage() {
  const held = createMemo(() => heldMessage(composer.held(), composer.view(), selected(), !!subagentOpen()));
  return (
    <Show when={held()}>{(h) => (
      <div class="act-queue act-held" role="status" aria-label={tr("Sends after compaction")}>
        <div class="act-queue-head"><Icon name="clock" size={15} /><b>{tr("Sends after compaction")}</b></div>
        <div class="act-queue-text">
          <div class="act-queue-message">
            <p class="wrap-pre">{h().text}</p>
            <Show when={h().attachments.length}>
              <div class="act-held-files"><For each={h().attachments}>{(name) => <span class="chip">{name}</span>}</For></div>
            </Show>
          </div>
        </div>
        <p class="act-held-note">{tr("The agent is compacting its context. This exact message is sent once, automatically, when compaction completes, and can no longer be cancelled. If it is not sent, it returns to the composer.")}</p>
      </div>
    )}</Show>
  );
}

/**
 * the messages hgs holds for this session until its
 * agent takes the previous one, one row each, oldest first, above the
 * composer; hidden when there are none. hgs sends them in order and a row
 * goes once it is sent. Cancel waits for hgs's answer; a cancelled message
 * comes back to an empty editor.
 */
export function HeldInputBar() {
  const rows = createMemo(() => heldInputs(composer.heldInputs(), composer.view(), selected(), !!subagentOpen()));
  const label = () => tr("Queued until %1 takes your previous message", providerName(selected()?.cmd ?? ""));
  return (
    <Show when={rows().length}>
      <div class="act-queue held-inputs" role="region" aria-label={tr("Held messages")}>
        <For each={rows()}>{(row) => (
          <div class="held-input">
            <Icon name="clock" size={14} />
            <span class="held-input-label ell" title={label()}>{label()}</span>
            <span class="held-input-preview ell" title={row.text}>{simplified(row.text)}</span>
            <button type="button" class="btn small" disabled={row.cancelling} title={tr("Cancel this message; hgs will not send it")}
              onClick={() => { void composer.cancelHeld(row.request_id); }}>{tr("Cancel")}</button>
          </div>
        )}</For>
      </div>
    </Show>
  );
}

/**
 * text left in the agent's own Terminal input holds
 * every send. Hidden until a draft is found; Send it presses Enter in
 * Terminal only while it still holds exactly the draft shown, and Open
 * Terminal shows it (DeepSeek opens its native UI). Zerus never types into,
 * edits or clears the draft.
 */
export function TerminalDraftBar() {
  const bar = createMemo(() => terminalDraftBar(composer.terminalDraft(), composer.view(), selected(), !!subagentOpen()));
  const openTerminal = () => {
    const s = selected();
    if (!s || computerOffline(s.computer)) return;
    if (s.cmd === "dsh") { nativeRoute(s)(); return; }
    if (!terminal.texts()?.supported) { notify("error", terminal.texts()?.reason || tr("Terminal is unavailable for this session."), true); return; }
    terminal.opened();
    setDetailTab("terminal");
  };
  return (
    <Show when={bar()}>{(b) => (
      <div class="act-queue terminal-draft" role="status" aria-label={tr("Unsent text in Terminal")}>
        <Icon name="terminal" size={14} />
        <span class="terminal-draft-text ell" title={b().draft}>
          {b().error ? trText(b().error!) : tr("Terminal has unsent text: “%1”", simplified(b().draft))}
        </span>
        <button type="button" class="btn small" disabled={!b().can_send} title={tr("Press Enter in Terminal: the agent gets this text as your message")}
          onClick={() => { void composer.submitDraft(); }}>{b().submitting ? tr("Sending…") : tr("Send it")}</button>
        <button type="button" class="btn small" onClick={openTerminal}>{tr("Open Terminal")}</button>
      </div>
    )}</Show>
  );
}
