import { describe, expect, it, vi } from "vitest";
import { createNotificationPreferences } from "./notification-preferences";
import type { DesktopService } from "../../../types";

const projection = (): DesktopService => ({ connected: true, epoch: "epoch", generation: 1, background: {},
  attention_counts: null, automatic_approvals: false, details_available: true, code: "",
  notification_preferences: { status: { abi: "desktop-notification-controls-v1", scope: { configuration: "a", service_epoch: "epoch" },
    journal: "journal", revision: 1, configured: true, preferences: { optional_questions: false } },
    pending: false, request_id: null, running: false, available: true, code: "" } });

describe("notification preference callsites", () => {
  it("keeps native mode's local setting and sends service edits only to the closed native command", async () => {
    let mode = false;
    const state = projection(), local = vi.fn(async () => {}), send = vi.fn(async () => {});
    const action = createNotificationPreferences(() => mode, () => state, () => true, local, send, vi.fn());
    expect(action.checked()).toBe(true);
    await action.change(false); expect(local).toHaveBeenCalledExactlyOnceWith(false); expect(send).not.toHaveBeenCalled();
    mode = true; expect(action.checked()).toBe(false);
    await action.change(true); expect(send).toHaveBeenCalledExactlyOnceWith("desktop_notification_preferences_set", { enabled: true });
    expect(local).toHaveBeenCalledOnce();
  });
  it("pending edits and reconnects use Check result without replay or a local fallback", async () => {
    const state = projection(), local = vi.fn(async () => {}), send = vi.fn(async () => {});
    state.notification_preferences!.pending = true;
    const action = createNotificationPreferences(() => true, () => state, () => true, local, send, vi.fn());
    await action.change(false); await action.change(true); expect(send).not.toHaveBeenCalled();
    await action.check(); await action.check();
    expect(send.mock.calls).toEqual([["desktop_notification_preferences_check"], ["desktop_notification_preferences_check"]]);
    state.connected = false; state.notification_preferences!.pending = false;
    await action.change(true); expect(send).toHaveBeenCalledTimes(2); expect(local).not.toHaveBeenCalled();
  });
  it("admits once before await and keeps a newer watch projection over an older invoke result", async () => {
    const state = projection(); let finish!: (value: unknown) => void;
    const send = vi.fn(() => new Promise(resolve => { finish = resolve; }));
    const action = createNotificationPreferences(() => true, () => state, () => false, vi.fn(), send, vi.fn());
    const first = action.change(true); await action.change(false); expect(send).toHaveBeenCalledOnce();
    state.notification_preferences!.status!.revision = 3;
    state.notification_preferences!.status!.preferences.optional_questions = true;
    finish({ ...projection().notification_preferences, status: { preferences: { optional_questions: false } } }); await first;
    expect(action.checked()).toBe(true); expect(state.notification_preferences!.status!.revision).toBe(3);
  });
  it("keeps an unavailable setting unavailable after a failed read", async () => {
    const state = projection(), report = vi.fn(), local = vi.fn(async () => {});
    state.notification_preferences!.available = false;
    const send = vi.fn(async () => { throw Error("unavailable"); });
    const action = createNotificationPreferences(() => true, () => state, () => true, local, send, report);
    await action.change(true); expect(send).not.toHaveBeenCalled();
    await action.check(); expect(report).toHaveBeenCalledOnce(); expect(action.disabled()).toBe(true);
    expect(local).not.toHaveBeenCalled();
  });
});
