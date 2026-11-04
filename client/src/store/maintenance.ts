import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { tr } from "../i18n";

export interface RecoveryForm { enabled: boolean; rules: Record<string, boolean>; session_limit_mode: string; delays: Record<string, string> }
export interface RecoveryEdit { policy: RecoveryForm | null; base: Record<string, unknown> }
/** Native supplies the closed shared DTO. The UI preserves the opaque update identity. */
export interface MaintenanceConfiguration { update: unknown; recovery: RecoveryEdit | null }
interface Intent { id: string; recoveryEdit: boolean }
interface Result extends Intent { state: "applied" | "not_applied" }
class SettledRefusal extends Error {}
interface Port {
  configuration(): Promise<MaintenanceConfiguration>;
  send(id: string, configuration: MaintenanceConfiguration): Promise<unknown>;
  read(): Promise<unknown>;
}

/** A single operation shared by Updates and Recovery. Checking never sends. */
export function createMaintenanceIntent(port: Port, id: () => string = () => crypto.randomUUID()) {
  const [pending, setPending] = createSignal<Intent>();
  const [busy, setBusy] = createSignal(false);
  const [result, setResult] = createSignal<Result>();
  const [error, setError] = createSignal("");
  const [unresolvedRead, setUnresolvedRead] = createSignal(false);
  const blocked = () => !!pending() || unresolvedRead();
  const settle = (value: Record<string, unknown>, request: Intent) => {
    if (value?.request_id !== request.id || value.op !== "maintenance_configure") {
      throw Error(tr("The background service has not confirmed this settings change."));
    }
    if (value.accepted === false && value.durable === false && value.pending === false) {
      setPending(undefined);
      setUnresolvedRead(false);
      if (value.code === "frontend_request_not_applied") {
        setResult({ ...request, state: "not_applied" });
        throw new SettledRefusal(tr("The previous change was not applied."));
      }
      throw new SettledRefusal(tr("Maintenance settings are unavailable from the desktop service."));
    }
    if (value.accepted !== true || value.durable !== true || value.pending !== false || value.configured !== true
      || (request.recoveryEdit ? value.recovery_save !== "saved" : value.recovery_save !== "not_requested")) {
      throw Error(tr("The background service has not confirmed this settings change."));
    }
    setPending(undefined);
    setUnresolvedRead(false);
    const next: Result = { ...request, state: "applied" };
    setResult(next);
    return next;
  };
  const recordError = (failure: unknown) => {
    const message = failure instanceof Error ? failure.message : String(failure);
    setError(message === "Maintenance settings are unavailable from the desktop service."
      ? tr("Maintenance settings are unavailable from the desktop service.")
      : message === "Could not save update preferences." ? tr("Could not save update preferences.") : message);
    throw failure;
  };
  return {
    pending, busy, result, error, blocked,
    async submit(edit?: RecoveryEdit, before?: () => Promise<void>) {
      if (busy() || blocked()) throw Error(tr("The previous settings change is unconfirmed. Check its result before making another change."));
      // Snapshot the form before any asynchronous native preference/data read.
      const snapshot = edit === undefined ? undefined : structuredClone(edit);
      const request = { id: id(), recoveryEdit: snapshot?.policy != null };
      setPending(request);
      setBusy(true);
      setError("");
      setResult(undefined);
      let dispatched = false;
      try {
        await before?.();
        const configuration = await port.configuration();
        if (!Object.hasOwn(configuration, "update") || !Object.hasOwn(configuration, "recovery")) {
          throw Error(tr("Maintenance settings are unavailable from the desktop service."));
        }
        if (snapshot !== undefined) configuration.recovery = snapshot;
        dispatched = true;
        const value = await port.send(request.id, configuration) as Record<string, unknown>;
        return settle(value, request);
      } catch (failure) {
        if (!dispatched) setPending(undefined);
        return recordError(failure);
      } finally { setBusy(false); }
    },
    async check() {
      if (busy()) return;
      setBusy(true);
      setError("");
      try {
        const value = await port.read() as Record<string, unknown>;
        let request = pending();
        if (!request && value?.op === "maintenance_configure" && value.pending === false
          && (value.request_id == null || value.accepted === false)) { setUnresolvedRead(false); return; }
        if (!request && value?.op === "maintenance_configure" && typeof value.request_id === "string" && typeof value.recovery_edit === "boolean") {
          request = { id: value.request_id, recoveryEdit: value.recovery_edit };
          setPending(request);
        }
        if (!request) throw Error(tr("The background service has not confirmed this settings change."));
        return settle(value, request);
      } catch (failure) {
        if (!pending() && !(failure instanceof SettledRefusal)) setUnresolvedRead(true);
        return recordError(failure);
      }
      finally { setBusy(false); }
    },
  };
}

export const maintenanceIntent = createMaintenanceIntent({
  configuration: () => invoke("desktop_service_maintenance_configuration"),
  send: (request_id, configuration) => invoke("desktop_service_maintenance_configure", { request_id, configuration }),
  read: () => invoke("desktop_service_frontend_result", { operation: "maintenance_configure" }),
});
