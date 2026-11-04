import { describe, expect, it, vi } from "vitest";
import { createMaintenanceIntent, type MaintenanceConfiguration, type RecoveryEdit } from "./maintenance";

const base = { order: [3, "fixture"], policy: { enabled: false } };
const configuration = (): MaintenanceConfiguration => ({ update: { gui: "native identity", channel: "stable", automatic: true }, recovery: { policy: null, base } });
const edit = (): RecoveryEdit => ({ policy: { enabled: true, rules: {}, session_limit_mode: "reset", delays: {} }, base: structuredClone(base) });
const receipt = (request_id: string, recovery_edit = false) => ({ request_id, op: "maintenance_configure", accepted: true, durable: true, pending: false, configured: true, recovery_edit, recovery_save: recovery_edit ? "saved" : "not_requested" });

describe("authenticated maintenance intents", () => {
  it("sends a full native configuration only after explicit preference completion and preserves the recovery counterpart", async () => {
    let preferred = "stable";
    const load = vi.fn(async () => ({ ...configuration(), update: { gui: "native identity", channel: preferred, automatic: true } }));
    const send = vi.fn(async (id: string, _value: MaintenanceConfiguration) => receipt(id));
    const read = vi.fn(async () => ({ op: "maintenance_configure", pending: false }));
    const intent = createMaintenanceIntent({ configuration: load, send, read }, () => "original");
    expect(load).not.toHaveBeenCalled();
    expect(send).not.toHaveBeenCalled();
    await intent.check();
    expect(load).not.toHaveBeenCalled();
    await intent.submit(undefined, async () => { await Promise.resolve(); preferred = "nightly"; });
    expect(send).toHaveBeenCalledExactlyOnceWith("original", { update: { gui: "native identity", channel: "nightly", automatic: true }, recovery: { policy: null, base } });
    expect(intent.blocked()).toBe(false);
  });
  it("snapshots a one-shot Recovery edit, retaining native update preferences without changing local policy", async () => {
    const form = edit();
    const load = vi.fn(async () => { form.policy!.enabled = false; return configuration(); });
    const send = vi.fn(async (id: string, _value: MaintenanceConfiguration) => receipt(id, true));
    const intent = createMaintenanceIntent({ configuration: load, send, read: vi.fn() }, () => "save");
    const result = await intent.submit(form);
    expect(send.mock.calls[0][1]).toEqual({ ...configuration(), recovery: { ...edit(), policy: { ...edit().policy, enabled: true } } });
    expect(result).toEqual({ id: "save", recoveryEdit: true, state: "applied" });
  });
  it("keeps an unknown Recovery save pending and only queries its original receipt across pages", async () => {
    const send = vi.fn(async (id: string) => ({ ...receipt(id, true), recovery_save: "unknown", pending: true }));
    const read = vi.fn(async () => receipt("original", true));
    const load = vi.fn(async () => configuration());
    const intent = createMaintenanceIntent({ configuration: load, send, read }, () => "original");
    await expect(intent.submit(edit())).rejects.toThrow("not confirmed");
    await expect(intent.submit()).rejects.toThrow("previous settings change");
    expect(intent.pending()).toEqual({ id: "original", recoveryEdit: true });
    await intent.check();
    expect(intent.pending()).toBeUndefined();
    expect(send).toHaveBeenCalledOnce();
    expect(load).toHaveBeenCalledOnce();
    expect(read).toHaveBeenCalledOnce();
  });
  it("restores native pending identity after reload and never submits while a read is uncertain", async () => {
    const read = vi.fn().mockRejectedValueOnce(Error("transport lost"))
      .mockResolvedValueOnce({ ...receipt("original", true), accepted: null, durable: false, pending: true, recovery_save: "unknown" })
      .mockResolvedValueOnce(receipt("original", true));
    const send = vi.fn();
    const intent = createMaintenanceIntent({ configuration: vi.fn(), send, read });
    await expect(intent.check()).rejects.toThrow("transport lost");
    expect(intent.blocked()).toBe(true);
    await expect(intent.submit()).rejects.toThrow("previous settings change");
    await expect(intent.check()).rejects.toThrow("not confirmed");
    expect(intent.pending()).toEqual({ id: "original", recoveryEdit: true });
    await intent.check();
    expect(intent.blocked()).toBe(false);
    expect(send).not.toHaveBeenCalled();
  });
  it("permits a new intent only after a native definitive not-applied refusal", async () => {
    let sequence = 0;
    const send = vi.fn(async (id: string) => { if (id === "id-1") throw Error("service restarting"); return receipt(id); });
    const read = vi.fn(async () => ({ request_id: "id-1", op: "maintenance_configure", accepted: false, durable: false, pending: false, code: "frontend_request_not_applied" }));
    const intent = createMaintenanceIntent({ configuration: async () => configuration(), send, read }, () => `id-${++sequence}`);
    await expect(intent.submit()).rejects.toThrow("service restarting");
    await expect(intent.check()).rejects.toThrow("previous change was not applied");
    expect(intent.blocked()).toBe(false);
    await intent.submit();
    expect(send.mock.calls.map(([id]) => id)).toEqual(["id-1", "id-2"]);
  });
  it("preserves the original after a mismatched receipt and releases local pre-send failure without sending", async () => {
    const send = vi.fn(async () => receipt("different"));
    const intent = createMaintenanceIntent({ configuration: async () => configuration(), send, read: vi.fn() }, () => "original");
    await expect(intent.submit()).rejects.toThrow("not confirmed");
    expect(intent.blocked()).toBe(true);
    const local = createMaintenanceIntent({ configuration: vi.fn(async () => { throw Error("configuration unavailable"); }), send, read: vi.fn() });
    await expect(local.submit()).rejects.toThrow("configuration unavailable");
    expect(local.blocked()).toBe(false);
    expect(send).toHaveBeenCalledOnce();
  });
});
