import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { DesktopService, NotificationOwnership } from "../types";
import { tr } from "../i18n";

/** Presence of this projection is the startup mode. It never grants writer authority. */
export function createDesktopService() {
  const [state, setState] = createSignal<DesktopService>();
  return {
    state, apply: (next: DesktopService) => setState(next),
    enabled: () => state() !== undefined,
    ready: (component: string) => !!state()?.connected && state()?.background[component]?.ready === true,
  };
}
export const desktopService = createDesktopService();
export const retryDesktopService = () => invoke<void>("desktop_service_retry");

export function pendingReadNotice(state: DesktopService | undefined): string {
  const transfer = state?.pending_read_import;
  if (!transfer?.required || transfer.complete) return "";
  return transfer.running
    ? tr("Transferring saved read marks. Marking sessions as read is paused.")
    : tr("Saved read marks could not be transferred. Marking sessions as read is paused. Retry to continue.");
}

export function retainedSourceNotice(state: DesktopService | undefined): string {
  const report = state?.retained_source_import;
  if (report?.creations_capture?.paused && report.creations_capture.code === "creation_source_private_slot") return tr("Session creation is paused. Check that the journal archive folder is available and private (0700 on Unix), then retry.");
  if (report?.creations_capture?.paused && report.creations_capture.code === "creation_source_data_directory") return tr("Session creation is paused. Check that the desktop data folder is available and owned by your user, then retry.");
  if (report?.creations_capture?.paused && report.creations_capture.code === "creation_source_unavailable") return tr("Session creation is paused. Check journal and archive access, ownership and private file permissions (0600 on Unix); review retained partial files before retrying.");
  if (report?.creations_capture?.paused) return tr("Session creation is paused while its original journal is preserved. Review migration status or retry.");
  if (!report?.required || !report.code || report.code === "retained_import_staged") return "";
  if (report.running) return tr("Preparing the migration report. Your sessions remain available.");
  if (report.code === "retained_import_handoff_busy") return tr("The migration report is deferred while background work is running.");
  return tr("The migration report needs review. Your sessions remain available.");
}

/** Called by the existing opt-in confirmation only. An uncertain request is not replayed. */
export function createServiceApprovalIntent(
  send: (id: string, enabled: boolean) => Promise<unknown>,
  id: () => string = () => crypto.randomUUID(),
  read?: () => Promise<unknown>,
) {
  const [uncertain, setUncertain] = createSignal<{ id: string; enabled: boolean }>();
  const [checking, setChecking] = createSignal(false);
  const settle = (result: Record<string, unknown>, request: { id: string; enabled: boolean }) => {
    if (result?.request_id === request.id && result.op === "automatic_approvals"
      && result.accepted === false && result.durable === false && result.pending !== true) {
      setUncertain(undefined);
      if (result.code === "frontend_request_not_applied") throw Error(tr("The previous change was not applied."));
      throw Error("Automatic approval settings are unavailable with this background service connection.");
    }
    if (result?.request_id !== request.id || result.op !== "automatic_approvals" || result.accepted !== true
      || result.durable !== true || result.enabled !== request.enabled || result.pending === true) {
      throw Error("The background service has not confirmed this approval setting.");
    }
    setUncertain(undefined);
  };
  return {
    pending: uncertain, checking,
    async submit(enabled: boolean) {
      if (uncertain()) throw Error("The previous approval setting is unconfirmed. Check the background service before making another change.");
      const request = { id: id(), enabled };
      setUncertain(request);
      const result = await send(request.id, enabled) as Record<string, unknown>;
      settle(result, request);
    },
    async check() {
      if (!read || checking()) return;
      setChecking(true);
      try {
        const result = await read() as Record<string, unknown>;
        let request = uncertain();
        if (!request && result?.op === "automatic_approvals" && result.pending === false
          && (result.request_id == null || result.accepted === false)) return;
        if (!request && result?.op === "automatic_approvals" && typeof result.request_id === "string" && typeof result.enabled === "boolean") {
          request = { id: result.request_id, enabled: result.enabled };
          setUncertain(request);
        }
        if (!request) throw Error("The background service has not confirmed this approval setting.");
        settle(result, request);
      } finally { setChecking(false); }
    },
  };
}
export const serviceApprovalIntent = createServiceApprovalIntent(
  (request_id, enabled) => invoke("desktop_service_automatic_approvals", { request_id, enabled }),
  () => crypto.randomUUID(),
  () => invoke("desktop_service_frontend_result", { operation: "automatic_approvals" }),
);


/** The native controller retains the original intent across connection changes.
 * Mount/Check only look it up; Rearm is an explicit user action. */
export function createOwnershipActions(
  rearm: () => Promise<NotificationOwnership>,
  read: () => Promise<NotificationOwnership>,
) {
  const [busy, setBusy] = createSignal(false);
  const act = async (call: () => Promise<NotificationOwnership>) => {
    if (busy()) return;
    setBusy(true);
    try { return await call(); } finally { setBusy(false); }
  };
  return { busy, rearm: () => act(rearm), check: () => act(read) };
}
export const ownershipActions = createOwnershipActions(
  () => invoke("desktop_service_ownership_rearm"),
  () => invoke("desktop_service_ownership_result"),
);
export function ownershipNotice(view: NotificationOwnership | undefined): string {
  if (!view) return "";
  if (view.pending) return tr("Background access is unconfirmed. Check the result before making another change.");
  if (view.status?.rearm_required) return tr("Background access was revoked. Resume it to let the service work while Zerus is closed.");
  if (view.code || view.status?.state !== "service" || !view.status.identity_current)
    return tr("Background access is unavailable. Retry to check the service.");
  return "";
}
