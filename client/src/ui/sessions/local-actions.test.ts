import { describe, expect, it } from "vitest";
import { DELETE_TEXT, DELETE_TITLE, RETRY_AFTER_SEND, RETRY_WHEN_READY, retryReason, routeInspect } from "./local-actions";

describe("a failed local row's Check (SessionsWindow.cpp messageAction 4477-4481)", () => {
  const ports = () => {
    const shown: string[] = [];
    return { shown, ports: { showTerminal: () => { shown.push("terminal"); }, showNativeUi: () => { shown.push("native"); }, notify: (tone: "info", text: string) => { shown.push(`${tone} ${text}`); } } };
  };

  it("leads to Terminal, Native UI or a refresh notice, and nowhere once its composer went", () => {
    const t = ports();
    routeInspect({ kind: "terminal" }, t.ports);
    routeInspect({ kind: "native_ui" }, t.ports);
    routeInspect({ kind: "refresh", notice: "Refreshing the native session. Check its provider configuration on studio if the error remains." }, t.ports);
    routeInspect(undefined, t.ports);
    expect(t.shown).toEqual([
      "terminal",
      "native",
      "info Refreshing the native session. Check its provider configuration on studio if the error remains.",
    ]);
  });

  it("Delete asks first", () => {
    expect([DELETE_TITLE, DELETE_TEXT]).toEqual(["Delete message?", "Remove this message from Activity? This cannot be undone."]);
  });

  it("Retry says why it is off while the composer is busy", () => {
    expect(retryReason({ sending: false, waiting: null }, false)).toBe("");
    expect(retryReason({ sending: true, waiting: null }, true)).toBe(RETRY_AFTER_SEND);
    expect(retryReason({ sending: false, waiting: "Waiting for the compaction to finish." }, true)).toBe(RETRY_AFTER_SEND);
    expect(retryReason({ sending: false, waiting: null }, true)).toBe(RETRY_WHEN_READY);
    expect(retryReason(undefined, false)).toBe("");
  });
});
