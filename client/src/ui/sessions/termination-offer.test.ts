import { describe, expect, it, vi } from "vitest";
import { readCleanupOffer, unavailableServiceRemoval, completeProgramList, PROGRAM_LIST_CHANGED, type CleanupOfferState } from "./termination-offer";

const target = { computer: "example", name: "codex/example/session", run_id: "run", conversation_id: "conversation" };
describe("Terminate dialog removal availability", () => {
  it.each([
    ["macOS", "termination_native_image_unavailable"],
  ])("visibly explains %s Service refusal without an automatic fallback", async (_platform, error) => {
    let state: CleanupOfferState = {};
    const calls: string[] = [];
    const offer = vi.fn(async () => { calls.push("offer"); throw error; });
    const cancel = vi.fn(async () => { calls.push("cancel"); });
    await readCleanupOffer(target, { offer, cancel }, () => true, (next) => { state = next; });
    expect(state).toEqual({ error });
    expect(state.offer).toBeUndefined();
    expect(unavailableServiceRemoval(state.error!)).toBe(true);
    expect(calls).toEqual(["offer"]); // No confirm/ordinary probe/fallback port exists.
    expect(cancel).not.toHaveBeenCalled();
  });
  it("does not classify the retired Linux blanket as a current platform limitation", async () => {
    let state: CleanupOfferState = {};
    const error = "termination_native_relay_unavailable";
    await readCleanupOffer(target, { offer: async () => { throw error; }, cancel: async () => {} }, () => true, (next) => { state = next; });
    expect(state).toEqual({ error });
    expect(unavailableServiceRemoval(error)).toBe(false);
  });
  it("keeps other offer errors visible and offers only the separate review", async () => {
    let state: CleanupOfferState = {};
    await readCleanupOffer(target, { offer: async () => { throw Error("The captured session changed."); }, cancel: async () => {} }, () => true, (next) => { state = next; });
    expect(state).toEqual({ error: "The captured session changed." });
    expect(unavailableServiceRemoval(state.error!)).toBe(false);
  });
  it("preserves real offers and cancels an offer arriving after close", async () => {
    let state: CleanupOfferState = {};
    const offered = { id: "original-offer", checked: true };
    const ports = { offer: async () => offered, cancel: vi.fn(async () => {}) };
    await readCleanupOffer(target, ports, () => true, (next) => { state = next; });
    expect(state).toEqual({ offer: offered });
    const publish = vi.fn();
    await readCleanupOffer(target, ports, () => false, publish);
    expect(ports.cancel).toHaveBeenCalledExactlyOnceWith("original-offer");
    expect(publish).not.toHaveBeenCalled();
  });
  it("keeps the whole native list and refuses malformed, duplicate, changed or truncated lists before confirmation", async () => {
    const programs = [{ pid: 7, label: "Editor" }, { pid: 9, label: "Terminal" }];
    expect(completeProgramList(programs)).toBe(true);
    expect(completeProgramList([])).toBe(true);
    expect(completeProgramList([{ pid: 1, label: "Namespace init" }])).toBe(false);
    expect(completeProgramList([{ pid: 0, label: "Invalid process" }])).toBe(false);
    const source = { id: "original-manifest-offer", checked: true, programs };
    let state: CleanupOfferState = {};
    await readCleanupOffer(target, { offer: async () => source, cancel: vi.fn(async () => {}) }, () => true, (next) => { state = next; });
    expect(state.offer?.programs).toEqual(programs);
    const bad = [
      [programs[0], programs[0]], [...programs].reverse(),
      [{ pid: 0, label: "replaced" }], [{ pid: 7.5, label: "partial" }],
      [{ pid: 7, label: "" }], [{ pid: 7, label: "unsafe\u0085name" }],
      [{ pid: 7, label: "é".repeat(257) }],
      Array.from({ length: 65 }, (_, i) => ({ pid: i + 2, label: "Editor" })),
    ];
    for (const programs of bad) {
      const cancel = vi.fn(async () => {});
      await readCleanupOffer(target, { offer: async () => ({ ...source, programs }), cancel }, () => true, (next) => { state = next; });
      expect(state).toEqual({ error: PROGRAM_LIST_CHANGED });
      expect(state.offer).toBeUndefined();
      expect(cancel).toHaveBeenCalledExactlyOnceWith(source.id);
    }
  });

});
