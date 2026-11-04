import { createSignal } from "solid-js";
import { accountEditOpen, accountEditApply, accountEditClose, type AccountEditKind, type AccountEditView, type AccountEditResult, type AccountEditValues } from "../bridge";
import { notify } from "./actions";

export interface AccountPrompt extends AccountEditView { origin: HTMLElement }
interface Port {
  open(account: string, profile: string, kind: AccountEditKind): Promise<AccountEditView>;
  apply(handle: string, values: AccountEditValues): Promise<AccountEditResult>;
  close(handle: string): Promise<void>;
  notice(error: boolean, text: string): void;
}
export function createAccountManagement(port: Port) {
  const [prompt, setPrompt] = createSignal<AccountPrompt>();
  const [busy, setBusy] = createSignal(false);
  const [notice, setNotice] = createSignal("");
  let generation = 0;
  const close = (expectedHandle?: string) => {
    const current = prompt();
    if (expectedHandle && current?.handle !== expectedHandle) return;
    generation++;
    setPrompt(undefined);
    if (current) void port.close(current.handle).catch(() => {});
  };
  const apply = async (view: AccountEditView, values: AccountEditValues = {}) => {
    setPrompt(undefined); setBusy(true);
    try {
      const answer = await port.apply(view.handle, values);
      answer.notices.forEach(text => { setNotice(text); port.notice(false, text); });
      answer.errors.forEach(text => port.notice(true, text));
    } catch (error) { port.notice(true, String(error)); }
    finally { setBusy(false); }
  };
  return {
    prompt, busy, close, notice, clearNotice: () => setNotice(""),
    async open(kind: AccountEditKind, account: string, profile: string, origin: HTMLElement) {
      if (busy()) return;
      setNotice("");
      close(); const current = generation; setBusy(true);
      try {
        const view = await port.open(account, profile, kind);
        if (current !== generation) { await port.close(view.handle); return; }
        if (kind === "default") await apply(view);
        else setPrompt({ ...view, origin });
      } catch (error) { if (current === generation) port.notice(true, String(error)); }
      finally { setBusy(false); }
    },
    async confirm(input: string | AccountEditValues = "", expectedHandle?: string) {
      const view = prompt();
      if (!view || busy() || (expectedHandle && view.handle !== expectedHandle)) return;
      const values=typeof input==="string" ? {label:input} : input;
      if ((view.kind === "rename" && values.label?.trim() === view.label)
        || (view.kind === "permissions" && values.mode === view.permission_mode)
        || (view.kind === "model" && values.model === view.models?.defaults.model && values.effort === view.models?.defaults.effort)) { close(); return; }
      await apply(view, values);
    },
  };
}
export const accountManagement = createAccountManagement({
  open: accountEditOpen, apply: accountEditApply, close: accountEditClose,
  // The mockup keeps successful account edits inside the selected card.
  // Qt's failures go through the existing sticky notice bus.
  notice: (error, text) => { if (error) notify("error", text, true); },
});
