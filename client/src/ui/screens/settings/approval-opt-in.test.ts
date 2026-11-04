import { createRoot } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import { APPROVALS_KEY, createApprovalOptIn } from "./approval-opt-in";
const origin = {} as HTMLElement;
describe("dangerous automatic approval opt-in", () => {
  it("never writes on opening or cancelling; enabling requires the explicit confirm", async () => {
    const save = vi.fn(async () => undefined);
    await createRoot(async (dispose) => {
      const state = createApprovalOptIn(save, vi.fn());
      await state.confirm();
      state.request(true, origin);
      expect(save).not.toHaveBeenCalled();
      state.cancel(); await state.confirm();
      expect(save).not.toHaveBeenCalled();
      state.request(true, origin); await state.confirm();
      expect(save).toHaveBeenCalledExactlyOnceWith(APPROVALS_KEY, true);
      expect(state.origin()).toBeUndefined(); dispose();
    });
  });
  it("disables immediately without a confirmation and suppresses duplicate submission", async () => {
    let finish!: () => void;
    const save = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    await createRoot(async (dispose) => {
      const state = createApprovalOptIn(save, vi.fn());
      state.request(false, origin); state.request(true, origin); await state.confirm();
      expect(save).toHaveBeenCalledExactlyOnceWith(APPROVALS_KEY, false);
      expect(state.origin()).toBeUndefined(); finish(); await Promise.resolve(); dispose();
    });
  });
  it("a failed save stays explicit and is never automatically replayed", async () => {
    const failed = vi.fn(), save = vi.fn(async () => { throw Error("private settings unavailable"); });
    await createRoot(async (dispose) => {
      const state = createApprovalOptIn(save, failed);
      state.request(true, origin); await state.confirm();
      expect(failed).toHaveBeenCalledOnce(); expect(save).toHaveBeenCalledOnce();
      state.cancel(); await state.confirm(); expect(save).toHaveBeenCalledOnce(); dispose();
    });
  });
});
