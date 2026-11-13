import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import type { JSX } from "solid-js";
import { tr } from "../../i18n";
import { AgentLogo, Icon } from "../lib/icons";
import { providerName } from "../lib/usage";
import { accountNameForProfile } from "../provider-selection";
import { sessionSwitchAccount, sessionSwitchPreview } from "../../bridge";
import { accountsPage, loadAccounts } from "../../store/accounts";
import { accountGroups, accountOf, consumeAccountSwitch, movable, moveOutcome, switchOutcome, switchRequest, takesSession } from "../../store/accountSwitch";
import { trText } from "../../text";
import type { Session } from "./types";

interface Choice { id: string; label: string; provider: string }
interface Preview { refusal: string; newSession: string; folder: string; loading: boolean }

/** The composer's account chip (Qt AccountSwitchChip): the session's agent
 * logo and account, every installed agent's usable accounts grouped by agent
 * (task 352), a confirmation with Cancel focused, then `hgs switch-account`.
 * An account of the same agent switches in place; one of another agent moves
 * the session there with a handoff. The receipt is shown in the chip. */
export function AccountSwitchChip(props: { session: Session; compact?: boolean; locked?: string; onSwitch?: () => void }): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const [target, setTarget] = createSignal<Choice>();
  const [preview, setPreview] = createSignal<Preview>({ refusal: "", newSession: "", folder: "", loading: false });
  const [busy, setBusy] = createSignal(false);
  const [notice, setNotice] = createSignal("");
  let dialog!: HTMLDialogElement;
  let chip!: HTMLButtonElement;

  const provider = () => props.session.accountProvider || props.session.agent || "";
  const current = () => accountOf(provider(), props.session.accountId ?? "");
  const groups = () => accountGroups(accountsPage(), props.session.computer, provider(), current());
  const currentRow = () => groups().find((group) => group.provider === provider())?.rows.find((row) => row.id === current());
  const label = () => accountNameForProfile(accountsPage(), currentRow()) || props.session.accountId || current();
  const others = () => groups().reduce((count, group) => count + group.rows.filter((row) => row.id !== current() || group.provider !== provider()).length, 0);
  const locked = () => props.locked ?? "";
  const crossing = () => !!target() && target()!.provider !== provider();
  createEffect(() => { if (consumeAccountSwitch(props.session.key)) { setOpen(true); void loadAccounts(); } });
  const close = (focus = false) => {
    setOpen(false);
    setTarget(undefined);
    if (dialog?.open) dialog.close();
    if (focus) chip?.focus();
  };
  onMount(() => {
    const outside = (event: PointerEvent) => {
      if (open() && !dialog?.open && !(event.target as Element).closest(".acct-switch-pop")) setOpen(false);
    };
    addEventListener("pointerdown", outside, true);
    onCleanup(() => removeEventListener("pointerdown", outside, true));
  });
  const ask = (choice: Choice) => {
    setOpen(false);
    setTarget(choice);
    setPreview({ refusal: "", newSession: "", folder: "", loading: choice.provider !== provider() });
    queueMicrotask(() => dialog?.showModal());
    if (choice.provider === provider()) return;
    // Task 352: the move names its new session and why it cannot run before
    // the owner confirms it.
    sessionSwitchPreview(props.session.name, choice.id)
      .then((answer) => {
        if (target()?.id !== choice.id) return;
        setPreview({ refusal: String(answer.refusal ?? ""), newSession: String(answer.new_session ?? ""), folder: String(answer.folder ?? ""), loading: false });
      })
      .catch((error) => { if (target()?.id === choice.id) setPreview({ refusal: String(error), newSession: "", folder: "", loading: false }); });
  };
  const move = async () => {
    const choice = target();
    if (!choice || busy() || (crossing() && (preview().loading || preview().refusal))) return;
    const across = choice.provider !== provider();
    const name = `${providerName(choice.provider)} ${choice.label || choice.id}`;
    setBusy(true);
    setNotice("");
    try {
      const receipt = await sessionSwitchAccount(props.session.name, choice.id, switchRequest());
      setNotice(across ? moveOutcome(receipt, "", name) : switchOutcome(receipt, "", choice.id));
      close();
      void loadAccounts(true);
    } catch (error) {
      setNotice(across ? moveOutcome({}, String(error), name) : switchOutcome({}, String(error), choice.id));
      close();
    } finally {
      setBusy(false);
      props.onSwitch?.();
    }
  };

  return (
    <Show when={movable(provider())}>
      <span class="acct-switch">
        <button ref={chip} type="button" class="tchip" classList={{ icon: props.compact, locked: !!locked() }}
          title={locked() || tr("Account: %1 (%2)", label(), providerName(provider()))} aria-haspopup="menu" aria-expanded={open()} aria-disabled={!!locked()}
          onClick={() => { if (locked()) return; setOpen(!open()); if (open()) void loadAccounts(); }}>
          <AgentLogo agent={provider()} size={13} fallback={<Icon name="accounts" size={13} />} />
          <Show when={!props.compact}><span class="ell">{locked() ? tr("%1 (locked)", label()) : label()}</span></Show>
        </button>
        <Show when={open() && !locked()}>
          <div class="acct-switch-pop" role="menu" aria-label={tr("Accounts on this computer")}>
            <Show when={others() > 0} fallback={<span class="dim acct-switch-empty">{tr("No other signed-in account.")}</span>}>
              <For each={groups()}>
                {(group) => (
                  <div class="acct-switch-group" role="group" aria-label={providerName(group.provider)}>
                    <div class="acct-switch-head">
                      <AgentLogo agent={group.provider} size={12} fallback={<Icon name="accounts" size={12} />} />
                      <span>{providerName(group.provider)}</span>
                    </div>
                    <For each={group.rows}>
                      {(option) => {
                        const own = () => group.provider === provider() && option.id === current();
                        const ready = () => takesSession(option);
                        const name = () => accountNameForProfile(accountsPage(), option) || option.id;
                        return (
                          <button type="button" role="menuitem" class="acct-switch-row" classList={{ blocked: !ready() }}
                            disabled={own() || !ready()} title={trText(option.detail)}
                            onClick={() => ask({ id: option.id, label: name(), provider: group.provider })}>
                            <AgentLogo agent={group.provider} size={12} fallback={<Icon name="accounts" size={12} />} />
                            <span class="ell">{name()}</span>
                            <Show when={own()}><Icon name="check" size={12} /></Show>
                            <Show when={!own() && !ready()}><span class="dim">{tr("Unavailable account")}</span></Show>
                            <Show when={!own() && ready() && group.provider === provider()}><span class="dim">{option.usage.status}</span></Show>
                            <Show when={!own() && ready() && group.provider !== provider()}><span class="dim">{tr("Move")}</span></Show>
                            <Show when={!ready()}><span class="acct-switch-why">{trText(option.detail)}</span></Show>
                          </button>
                        );
                      }}
                    </For>
                  </div>
                )}
              </For>
            </Show>
            <Show when={notice()}><p class="acct-switch-notice" role="status">{notice()}</p></Show>
          </div>
        </Show>
      </span>
      <Show when={target()}>
        {(choice) => (
          <dialog ref={dialog} class="dlg" aria-labelledby="acct-switch-title"
            onCancel={(event) => { event.preventDefault(); close(true); }} onClose={() => setTarget(undefined)}>
            <Show when={crossing()} fallback={
              <>
                <div class="dlg-head"><Icon name="accounts" size={18} /><h2 id="acct-switch-title">{tr("Move this session to another account?")}</h2></div>
                <div class="dlg-body">
                  <p>{tr("Move this session from %1 to %2?", label(), choice().label || choice().id)}</p>
                  <p class="muted">{tr("The agent stops and starts again in the same conversation, folder and name. A failed start returns to the old account; an unconfirmed one is never retried.")}</p>
                </div>
              </>
            }>
              <div class="dlg-head">
                <AgentLogo agent={choice().provider} size={18} fallback={<Icon name="accounts" size={18} />} />
                <h2 id="acct-switch-title">{tr("Move this session to %1?", providerName(choice().provider))}</h2>
              </div>
              <div class="dlg-body acct-move-body">
                <p>{tr("Move this conversation from %1 (%2) to %3 (%4)?", providerName(provider()), label(), providerName(choice().provider), choice().label || choice().id)}</p>
                <p>{tr("%1 cannot resume %2's conversation, so Zerus starts a new %1 session in the same folder and gives it a handoff. The old session stops and stays readable; the two are linked.", providerName(choice().provider), providerName(provider()))}</p>
                <p class="acct-move-list"><b>{tr("Carried over:")}</b> {tr("a summary, the open tasks, the recent turns and the attachments as references.")}</p>
                <p class="acct-move-list"><b>{tr("Not carried over:")}</b> {tr("%1's own tool state, its prompt cache and the permissions chosen for this session.", providerName(provider()))}</p>
                <Show when={preview().newSession}><p class="muted">{tr("New session: %1", preview().newSession)}</p></Show>
                <Show when={preview().loading}><p class="muted" role="status">{tr("Checking the move…")}</p></Show>
                <Show when={preview().refusal}><p class="acct-move-refusal" role="alert">{tr("Cannot move: %1", preview().refusal.replace(/^hgs:\s*/, ""))}</p></Show>
              </div>
            </Show>
            <div class="dlg-foot"><span class="spacer" />
              <button type="button" class="btn" autofocus onClick={() => close(true)}>{tr("Cancel")}</button>
              <button type="button" class="btn primary" disabled={busy() || (crossing() && (preview().loading || !!preview().refusal))}
                onClick={() => void move()}>{crossing() ? tr("Move session") : tr("Move account")}</button>
            </div>
          </dialog>
        )}
      </Show>
    </Show>
  );
}
