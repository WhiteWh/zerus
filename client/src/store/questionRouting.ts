import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import * as bridge from "../bridge";
import { bus } from "./notices";
import { computerOffline } from "./catalog";
import { desktopService } from "./desktopService";
import { errorText } from "./actions";
import type { Session } from "../ui/sessions/types";
import { tr, trNoop } from "../i18n";

/**
 * The person's switches of the owner-approved question route (zerus-sa9qn.32):
 * whether a supervised child's blocking question goes to its parent session.
 * The per-child switch is kept on the child's computer (`hgs question-routing`);
 * the global one is kept by the background service. Neither answers anything.
 */
export const SEND_ON = trNoop("This child's blocking questions are sent to its parent session.");
export const SEND_OFF = trNoop("This child's questions are not sent to its parent session.");
export const SEND_UNKNOWN = trNoop("Update Zerus on this child's computer to choose where its questions go.");
export const SEND_OFFLINE = trNoop("The child's computer is offline.");
export const GLOBAL_LABEL = trNoop("Send child questions to the parent session");
export const GLOBAL_HINT = trNoop("On by default. A blocking question of a supervised child session is sent once to its parent session, which can answer it or ask you. Approvals, folder trust and sign-in always come to you. Turning this off stops new sends; questions already sent stay sent.");
export const GLOBAL_UNCONFIRMED = trNoop("The background service has not confirmed this setting.");
// The core's refusals of a change (client/core/src/question_routing.rs).
trNoop("Update Zerus on this computer to choose where a child's questions go.");
trNoop("The computer did not confirm where this child's questions go. Refresh the session.");
trNoop("Where this child's questions go changed on another device. Refresh the session and try again.");
trNoop("This session has no durable identity, so its questions cannot be routed.");
trNoop("Only you can change where a child's questions go.");

export interface Routing { sendToParent: boolean; revision: number }

const [overrides, setOverrides] = createSignal<ReadonlyMap<string, Routing>>(new Map());
const [changing, setChanging] = createSignal<ReadonlySet<string>>(new Set());

/** The row's switch, or a confirmed change of a higher revision the listing has not shown yet. */
export function routingOf(s: Session): Routing | undefined {
  const row = s.questionRouting;
  const confirmed = overrides().get(s.key);
  return confirmed && (!row || confirmed.revision > row.revision) ? confirmed : row;
}

/** Only a Claude child routes in this version (Codex has nothing to route). */
export const routable = (s: Session) => !!s.parent && s.state !== "archived" && s.cmd === "claude";

/** Turns this child's switch the other way, once; the host confirms the value and revision. */
export async function toggleRouting(s: Session): Promise<void> {
  const current = routingOf(s);
  if (!current || changing().has(s.key) || computerOffline(s.computer)) return;
  const target = { computer: s.computer, name: s.name, run_id: s.runId ?? "", conversation_id: s.conversationId ?? "" };
  setChanging((all) => new Set(all).add(s.key));
  try {
    const confirmed = await bridge.setQuestionRouting(target, !current.sendToParent, current.revision);
    const value = confirmed as { send_to_parent?: unknown; revision?: unknown };
    if (typeof value.send_to_parent === "boolean" && typeof value.revision === "number") {
      const next = { sendToParent: value.send_to_parent, revision: value.revision };
      setOverrides((all) => new Map(all).set(s.key, next));
    }
  } catch (error) {
    bus.notify("error", tr(errorText(error)), true);
  } finally {
    setChanging((all) => { const next = new Set(all); next.delete(s.key); return next; });
  }
}

/** The check item of a child's Session actions, or undefined when it does not apply. */
export function routingEntry(s: Session) {
  if (!routable(s) || !s.parent) return undefined;
  const routing = routingOf(s);
  const offline = computerOffline(s.computer);
  const hint = offline ? tr(SEND_OFFLINE) : !routing ? tr(SEND_UNKNOWN) : routing.sendToParent ? tr(SEND_ON) : tr(SEND_OFF);
  return {
    label: tr("Send questions to %1", s.parent.name.split("/").pop() || s.parent.name),
    icon: "send",
    checked: routing?.sendToParent ?? false,
    disabled: !routing || offline || changing().has(s.key),
    hint,
    action: () => { void toggleRouting(s); },
  };
}

const [globalBusy, setGlobalBusy] = createSignal(false);
export { globalBusy };

/** The service's global switch, or undefined where nothing routes. */
export const globalRouting = (): boolean | undefined => {
  const value = desktopService.state()?.question_routes;
  return typeof value === "boolean" ? value : undefined;
};

/** One request of the person's global switch through the native frontend lane. */
export async function setGlobalRouting(enabled: boolean): Promise<void> {
  if (globalBusy()) return;
  setGlobalBusy(true);
  try {
    const reply = await invoke<Record<string, unknown>>("desktop_service_question_routes", { request_id: crypto.randomUUID(), enabled });
    if (reply?.accepted !== true || reply.enabled !== enabled) bus.notify("error", tr(GLOBAL_UNCONFIRMED), true);
  } catch (error) {
    bus.notify("error", tr(errorText(error)), true);
  } finally {
    setGlobalBusy(false);
  }
}
