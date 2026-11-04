import { describe, expect, it, vi } from "vitest";
import { createDesktopService, createServiceApprovalIntent, pendingReadNotice, retainedSourceNotice, ownershipNotice, createOwnershipActions } from "./desktopService";
import { createApprovalOptIn } from "../ui/screens/settings/approval-opt-in";
import { emptyBatch, isEmptyBatch, type DesktopService, type NotificationOwnership } from "../types";
import { normalize } from "../bridge";

export const serviceView = (patch: Partial<DesktopService> = {}): DesktopService => ({
  connected: true, epoch: "epoch", generation: 1, background: {}, attention_counts: null,
  automatic_approvals: null, details_available: false, code: "background_unready", ...patch,
});
describe("desktop service presentation", () => {
  it("shows deferred or unconfirmed migration without changing connected readiness", () => {
    const report = { required: true, complete: false, running: false, code: "retained_import_handoff_busy", phase: "not_committed" };
    const service = createDesktopService();
    service.apply(serviceView({ background: { fleet: { ready: true, code: "ready" } }, retained_source_import: report }));
    expect(retainedSourceNotice(service.state())).toContain("deferred while background work is running");
    expect(service.ready("fleet")).toBe(true);
    expect(retainedSourceNotice(serviceView({ retained_source_import: { ...report, code: "retained_import_source_kept_for_review", phase: "quarantined" } }))).toContain("needs review");
    expect(retainedSourceNotice(serviceView({ retained_source_import: { ...report, complete: true, code: "retained_import_result_unavailable", phase: "published" } }))).toContain("needs review");
    expect(retainedSourceNotice(serviceView({ retained_source_import: { ...report, complete: true, code: "retained_import_staged", phase: "published" } }))).toBe("");
    expect(retainedSourceNotice(serviceView({ retained_source_import: { ...report, required: false } }))).toBe("");
    expect(retainedSourceNotice(serviceView({ retained_source_import: { ...report, required: false, complete: true, code: "retained_import_staged", creations_capture: { code: "creation_source_unavailable", paused: true, capture_id: null } } }))).toContain("Session creation is paused");
  });
  it("explains the archive and data-directory refusals without changing readiness", () => {
    const report = { required: false, complete: true, running: false, code: "retained_import_staged", phase: "published" };
    const view = (code: string) => serviceView({ retained_source_import: { ...report, creations_capture: { code, paused: true, capture_id: null } } });
    expect(retainedSourceNotice(view("creation_source_private_slot"))).toContain("0700 on Unix");
    expect(retainedSourceNotice(view("creation_source_data_directory"))).toContain("owned by your user");
    expect(retainedSourceNotice(view("creation_source_unavailable"))).toContain("0600 on Unix");
    expect(view("creation_source_private_slot").connected).toBe(true);
  });
  it("shows blocked read transfer and its existing Retry path until durable completion", () => {
    expect(pendingReadNotice(undefined)).toBe("");
    const pending = { required: true, complete: false, running: true, code: "pending_read_import_waiting", through: 0, superseded: 0, delivered: false as const };
    expect(pendingReadNotice(serviceView({ pending_read_import: pending }))).toContain("Transferring saved read marks");
    expect(pendingReadNotice(serviceView({ pending_read_import: { ...pending, running: false } }))).toContain("Retry to continue");
    expect(pendingReadNotice(serviceView({ pending_read_import: { ...pending, running: false, complete: true, superseded: 1 } }))).toBe("");
  });
  it("is off until the native startup mode publishes a projection; IPC alone enables no component", () => {
    const service = createDesktopService();
    expect(service.enabled()).toBe(false);
    expect(service.ready("reads")).toBe(false);
    service.apply(serviceView());
    expect(service.enabled()).toBe(true);
    expect(service.ready("reads")).toBe(false);
    service.apply(serviceView({ background: { reads: { ready: true, code: "ready" } } }));
    expect(service.ready("reads")).toBe(true);
    expect(service.ready("approvals")).toBe(false);
    service.apply(serviceView({ connected: false }));
    expect(service.enabled()).toBe(true);
    expect(service.ready("reads")).toBe(false);
  });
  it("delivers service-only batches and preserves incomplete/null count provenance", () => {
    const projection = serviceView({ attention_counts: { epoch: "epoch", scope: "shared_host_state", display_only: true, authority: false, local_marks_included: false,
      aggregate: { complete: false, attention: null, total: null, known_attention: 0, known_total: 3 }, computers: [] } });
    const batch = normalize({ ...emptyBatch(), desktop_service: projection });
    expect(isEmptyBatch(batch)).toBe(false);
    expect(batch.desktop_service).toEqual(projection);
    expect(batch.desktop_service?.attention_counts?.aggregate.attention).toBeNull();
  });
});
describe("service approval intents", () => {
  const origin = {} as HTMLElement;
  it("uses the existing enable confirmation before its native seam and never writes a setting", async () => {
    const send = vi.fn(async (request_id: string, enabled: boolean) => ({ request_id, enabled, op: "automatic_approvals", accepted: true, durable: true }));
    const intent = createServiceApprovalIntent(send, () => "request-1");
    const confirmation = createApprovalOptIn((_key, enabled) => intent.submit(enabled), vi.fn());
    confirmation.request(true, origin);
    expect(send).not.toHaveBeenCalled();
    confirmation.cancel(); await confirmation.confirm();
    expect(send).not.toHaveBeenCalled();
    confirmation.request(true, origin); await confirmation.confirm();
    expect(send).toHaveBeenCalledExactlyOnceWith("request-1", true);
  });
  it("retains an uncertain UUID and refuses a replay, including another confirm", async () => {
    const send = vi.fn(async () => { throw Error("transport closed"); });
    const intent = createServiceApprovalIntent(send, () => "request-1");
    await expect(intent.submit(true)).rejects.toThrow("transport closed");
    expect(intent.pending()).toEqual({ id: "request-1", enabled: true });
    await expect(intent.submit(true)).rejects.toThrow("unconfirmed");
    await expect(intent.submit(false)).rejects.toThrow("unconfirmed");
    expect(send).toHaveBeenCalledOnce();
  });
  it("rejects non-durable or mismatched receipts and permits a fresh request after a known refusal", async () => {
    for (const reply of [{ request_id: "request-1", accepted: true, durable: false }, { request_id: "other", accepted: true, durable: true }]) {
      const intent = createServiceApprovalIntent(async () => reply, () => "request-1");
      await expect(intent.submit(true)).rejects.toThrow("not confirmed");
      expect(intent.pending()?.id).toBe("request-1");
    }
    const send = vi.fn(async (request_id: string) => ({ request_id, op: "automatic_approvals", accepted: false, durable: false }));
    const intent = createServiceApprovalIntent(send, () => "request-1");
    await expect(intent.submit(true)).rejects.toThrow("unavailable");
    expect(intent.pending()).toBeUndefined();
  });
  it("checks the original receipt without sending again and restores uncertainty after a page reload", async () => {
    const send = vi.fn(async () => { throw Error("lost response"); });
    const read = vi.fn(async () => ({ request_id: "request-1", op: "automatic_approvals", enabled: true, accepted: true, durable: true, pending: false }));
    const intent = createServiceApprovalIntent(send, () => "request-1", read);
    await expect(intent.submit(true)).rejects.toThrow("lost response");
    await intent.check();
    expect(send).toHaveBeenCalledOnce();
    expect(read).toHaveBeenCalledOnce();
    expect(intent.pending()).toBeUndefined();
    const missing = vi.fn(async () => ({ request_id: "native-original", op: "automatic_approvals", enabled: false, accepted: null, durable: false, pending: true }));
    const reloaded = createServiceApprovalIntent(send, () => "fresh-id", missing);
    await expect(reloaded.check()).rejects.toThrow("not confirmed");
    expect(reloaded.pending()).toEqual({ id: "native-original", enabled: false });
    await expect(reloaded.submit(true)).rejects.toThrow("unconfirmed");
    expect(send).toHaveBeenCalledOnce();
  });
  it("allows turning approvals off after native confirmed-exit receipt lookup proves the first request was not applied", async () => {
    let sequence = 0;
    const send = vi.fn(async (request_id: string, enabled: boolean) => {
      if (request_id === "request-1") throw Error("service restarting");
      return { request_id, enabled, op: "automatic_approvals", accepted: true, durable: true };
    });
    const read = vi.fn(async () => ({ request_id: "request-1", op: "automatic_approvals", accepted: false, durable: false, pending: false, code: "frontend_request_not_applied" }));
    const intent = createServiceApprovalIntent(send, () => `request-${++sequence}`, read);
    await expect(intent.submit(true)).rejects.toThrow("service restarting");
    await expect(intent.submit(false)).rejects.toThrow("unconfirmed");
    await expect(intent.check()).rejects.toThrow("The previous change was not applied.");
    expect(intent.pending()).toBeUndefined();
    await intent.submit(false);
    expect(send.mock.calls).toEqual([["request-1", true], ["request-2", false]]);
    expect(read).toHaveBeenCalledOnce();
  });

});


describe("notification ownership presentation", () => {
  const view = (patch: Partial<NotificationOwnership> = {}): NotificationOwnership => ({
    status: { abi: "desktop-ownership-v1", configuration: "a".repeat(64), journal: "journal", revision: 3,
      state: "revoked", rearm_required: true, request_id: null, owner_epoch: null, identity_current: false },
    request_id: null, pending: false, running: false, code: "", ...patch,
  });
  it("shows explicit rearm after revoke and keeps unknown intents on Check result", () => {
    expect(ownershipNotice(view())).toContain("revoked");
    expect(ownershipNotice(view({ pending: true }))).toContain("unconfirmed");
    expect(ownershipNotice(undefined)).toBe("");
    const updated = view(); updated.status = { ...updated.status!, state: "identity_changed", rearm_required: false };
    expect(ownershipNotice(updated)).toContain("unavailable");
    expect(ownershipNotice(updated)).not.toContain("revoked");
    const current = view(); current.status = { ...current.status!, state: "service", rearm_required: false, identity_current: true };
    expect(ownershipNotice(current)).toBe("");
  });
  it("mount and result lookups never grant or rearm; only the explicit action calls its native route", async () => {
    const send = vi.fn(async () => view());
    const read = vi.fn(async () => view({ pending: true }));
    const actions = createOwnershipActions(send, read);
    expect(send).not.toHaveBeenCalled();
    expect(read).not.toHaveBeenCalled();
    await actions.check(); await actions.check();
    expect(read).toHaveBeenCalledTimes(2);
    expect(send).not.toHaveBeenCalled();
    await actions.rearm();
    expect(send).toHaveBeenCalledExactlyOnceWith();
  });
  it("serializes explicit clicks while retaining a failed result as native-owned uncertainty", async () => {
    let release!: () => void;
    const send = vi.fn(() => new Promise<NotificationOwnership>(resolve => { release = () => resolve(view({ pending: true })); }));
    const read = vi.fn(async () => { throw Error("disconnected"); });
    const actions = createOwnershipActions(send, read);
    const first = actions.rearm();
    expect(actions.busy()).toBe(true);
    await actions.rearm(); await actions.check();
    expect(send).toHaveBeenCalledOnce(); expect(read).not.toHaveBeenCalled();
    release(); await first;
    await expect(actions.check()).rejects.toThrow("disconnected");
    expect(send).toHaveBeenCalledOnce();
    expect(actions.busy()).toBe(false);
  });
});
