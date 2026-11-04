import { createSignal } from "solid-js";
import { trNoop } from "../../../i18n";
export const APPROVALS_KEY = "automaticApprovals/enabled";
export const APPROVALS_TITLE = trNoop("Enable automatic approvals?");
export const APPROVALS_CONFIRM = trNoop("Enable automatic approvals");
export const APPROVALS_BODY = trNoop("Agents on this computer and connected machines will be allowed to run tools without your review, including commands that delete files or consume paid usage. Resuming after an expired cache may process the full conversation again. Zerus will grant one-time permissions and record each attempt in this computer's private journal. Enable this dangerous mode?");
export const APPROVALS_WARNING = trNoop("Allows agents to run commands and change files without waiting for you, including destructive actions and paid usage. Resuming after an expired cache may process the full conversation again. Applies to all reachable sessions while this Zerus is running, including with its window hidden. Ordinary questions, folder trust and permanent permission changes still need you.");
export const APPROVALS_HINT = trNoop("Off by default and separate from automatic recovery. Supports verified one-time Claude, OpenCode and DeepSeek tool permissions. A journal entry must be saved first. Unconfirmed delivery is never retried automatically. Turning this off stops new approvals; requests already sent may still finish.");
export function createApprovalOptIn(save: (key: string, value: boolean) => Promise<unknown>, failed: (error: unknown) => void) {
  const [origin, setOrigin] = createSignal<HTMLElement>();
  const [busy, setBusy] = createSignal(false);
  const apply = async (enabled: boolean) => {
    if (busy()) return;
    setBusy(true);
    try { await save(APPROVALS_KEY, enabled); setOrigin(undefined); }
    catch (error) { failed(error); }
    finally { setBusy(false); }
  };
  return { origin, busy, cancel: () => { if (!busy()) setOrigin(undefined); },
    request: (enabled: boolean, element: HTMLElement) => { if (busy()) return; if (enabled) setOrigin(element); else void apply(false); },
    confirm: () => origin() ? apply(true) : Promise.resolve(),
  };
}
