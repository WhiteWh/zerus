import { createRoot } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import produced from "../../../../fixtures/desktop-delivery-status.json";
import { createServiceApprovalIntent } from "../../../store/desktopService";
import { APPROVALS_KEY, createApprovalOptIn } from "./approval-opt-in";
import { canToggleServiceApproval } from "./approval-readiness";

// Extracted verbatim by generate-delivery-status-fixture.py from the production
// root projection/decorator regression, rather than authored ready booleans.
const view = (name: string) => {
  const status = produced.find(row => row.case === name)!.status;
  return { connected: true, automatic_approvals: status.automatic_approvals, background: status.lifecycle.background };
};
const origin = {} as HTMLElement;

describe("service approval switch direction", () => {
  it("can turn a persisted ON off while the real root approval component is degraded", async () => {
    const on = view("approval_degraded");
    expect(on.automatic_approvals).toBe(true);
    expect(on.background.approvals.ready).toBe(false);
    expect(canToggleServiceApproval(on)).toBe(true);
    const send = vi.fn(async (request_id: string, enabled: boolean) => ({ request_id, enabled, op: "automatic_approvals", accepted: true, durable: true }));
    const failed = vi.fn();
    await createRoot(async dispose => {
      const intent = createServiceApprovalIntent(send, () => "disable-original");
      const save = vi.fn((_key: string, enabled: boolean) => intent.submit(enabled));
      const confirmation = createApprovalOptIn(save, failed);
      confirmation.request(false, origin);
      await Promise.resolve();
      expect(save).toHaveBeenCalledExactlyOnceWith(APPROVALS_KEY, false);
      expect(send).toHaveBeenCalledExactlyOnceWith("disable-original", false);
      expect(confirmation.origin()).toBeUndefined();
      expect(failed).not.toHaveBeenCalled();
      dispose();
    });
    expect(canToggleServiceApproval(view("disabled_while_degraded"))).toBe(false);
  });
  it("keeps ON behind its readiness and existing confirmation, including after OFF", async () => {
    const off = view("disabled_but_ready");
    expect(off.automatic_approvals).toBe(false);
    expect(canToggleServiceApproval(off)).toBe(true);
    await createRoot(async dispose => {
      const save = vi.fn(async () => undefined);
      const confirmation = createApprovalOptIn(save, vi.fn());
      confirmation.request(true, origin);
      expect(save).not.toHaveBeenCalled();
      await confirmation.confirm();
      expect(save).toHaveBeenCalledExactlyOnceWith(APPROVALS_KEY, true);
      dispose();
    });
    expect(canToggleServiceApproval(view("awaiting_handoff"))).toBe(false);
    expect(canToggleServiceApproval(view("drained"))).toBe(false);
    expect(canToggleServiceApproval({ ...view("approval_degraded"), connected: false })).toBe(false);
    expect(canToggleServiceApproval(undefined)).toBe(false);
  });
});
