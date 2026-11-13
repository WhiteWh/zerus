// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";

vi.mock("../lib/icons", () => ({ Icon: () => "icon" }));
vi.mock("../sessions/SessionCard", () => ({ ProviderGlyph: () => "glyph" }));
vi.mock("../../store/catalog", () => ({ agentKind: () => "claude" }));
vi.mock("../sessions/actions-ui", () => ({ menuItemActivates: () => true }));
vi.mock("../../store/ui", () => ({ menu: () => undefined, visibleMenu: () => undefined, setMenu: vi.fn() }));
vi.mock("../../i18n", () => ({ tr: (text: string) => text }));

import { ERROR_NOTICE_MS, NOTICE_MS, bus } from "../../store/notices";
import { Toasts } from "./Overlays";

let dispose: (() => void) | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  for (const notice of bus.notices()) bus.dismiss(notice.id);
});
afterEach(() => {
  dispose?.();
  dispose = undefined;
  document.body.replaceChildren();
  vi.useRealTimers();
});
function mount() {
  const host = document.createElement("div");
  document.body.append(host);
  dispose = render(() => createComponent(Toasts, {}), host);
  return host;
}
const toast = (host: HTMLElement) => host.querySelector<HTMLElement>(".toast");

it("dismisses an error after its longer timeout", () => {
  const host = mount();
  bus.notify("error", "Stop failed. Open Terminal to check the agent.", true);
  expect(toast(host)?.textContent).toContain("Stop failed");
  vi.advanceTimersByTime(ERROR_NOTICE_MS - 1);
  expect(toast(host)).not.toBeNull();
  vi.advanceTimersByTime(1);
  expect(toast(host)).toBeNull();
});

it("holds an error's timer while hovered and resumes the rest on leave", () => {
  const host = mount();
  bus.notify("error", "Stop failed.", true);
  vi.advanceTimersByTime(ERROR_NOTICE_MS - 1_000);
  const el = toast(host)!;
  el.dispatchEvent(new Event("pointerenter"));
  vi.advanceTimersByTime(60_000);
  expect(toast(host)).not.toBeNull();
  el.dispatchEvent(new Event("pointerleave"));
  vi.advanceTimersByTime(999);
  expect(toast(host)).not.toBeNull();
  vi.advanceTimersByTime(1);
  expect(toast(host)).toBeNull();
});

it("holds an error's timer while its close button has focus", () => {
  const host = mount();
  bus.notify("error", "Stop failed.", true);
  const close = toast(host)!.querySelector<HTMLButtonElement>(".dismiss")!;
  close.focus();
  vi.advanceTimersByTime(ERROR_NOTICE_MS * 2);
  expect(toast(host)).not.toBeNull();
  close.blur();
  vi.advanceTimersByTime(ERROR_NOTICE_MS);
  expect(toast(host)).toBeNull();
});

it("keeps the close button and dismisses an error at once", () => {
  const host = mount();
  bus.notify("error", "Stop failed.", true);
  toast(host)!.querySelector<HTMLButtonElement>(".dismiss")!.click();
  expect(toast(host)).toBeNull();
});

it("keeps the design's shorter time and no close button for an info toast", () => {
  const host = mount();
  bus.notify("info", "Copied");
  expect(toast(host)?.querySelector(".dismiss")).toBeNull();
  vi.advanceTimersByTime(NOTICE_MS - 1);
  expect(toast(host)).not.toBeNull();
  vi.advanceTimersByTime(1);
  expect(toast(host)).toBeNull();
});
