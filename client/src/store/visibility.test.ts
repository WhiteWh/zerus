import { describe, expect, it, vi } from "vitest";
import { anyHidden, focusMotionVisibility, nativeFocusVisibility, type PageVisibility } from "./visibility";

function source(initial = false) {
  let hidden = initial;
  const listeners = new Set<() => void>();
  const visibility: PageVisibility = {
    hidden: () => hidden,
    onChange(listener) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
  };
  return {
    visibility,
    listeners,
    setHidden(value: boolean) {
      hidden = value;
      for (const listener of [...listeners]) listener();
    },
  };
}

describe("anyHidden", () => {
  it("keeps focus pausing Linux-only and separate from document visibility", () => {
    const doc = source(), focus = source();
    const linux = focusMotionVisibility(doc.visibility, focus.visibility, true);
    const other = focusMotionVisibility(doc.visibility, focus.visibility, false);
    focus.setHidden(true);
    expect([doc.visibility.hidden(), linux.hidden(), other.hidden()]).toEqual([false, true, false]);
    doc.setHidden(true);
    expect([linux.hidden(), other.hidden()]).toEqual([true, true]);
  });

  it("uses toplevel focus even when the webview loses focus to a native child", async () => {
    let changed!: (event: { payload: boolean }) => void;
    const stop = vi.fn();
    const doc = source();
    const native = nativeFocusVisibility({
      isFocused: async () => true,
      onFocusChanged: async (listener) => { changed = listener; return stop; },
    });
    const combined = focusMotionVisibility(doc.visibility, native, true);
    let changes = 0;
    const off = combined.onChange(() => changes++);
    await Promise.resolve(); await Promise.resolve();
    // Only native toplevel events feed this source, so document blur does not pause it.
    expect(combined.hidden()).toBe(false);
    changed({ payload: false });
    expect(combined.hidden()).toBe(true);
    changed({ payload: true });
    expect(combined.hidden()).toBe(false);
    expect(changes).toBe(2);
    off();
    expect(stop).toHaveBeenCalledOnce();
  });

  it("ignores a stale initial focus query after a newer native event", async () => {
    let changed!: (event: { payload: boolean }) => void;
    let resolve!: (focused: boolean) => void;
    const focus = nativeFocusVisibility({
      isFocused: () => new Promise<boolean>((done) => { resolve = done; }),
      onFocusChanged: async (listener) => { changed = listener; return () => {}; },
    });
    const off = focus.onChange(() => {});
    await Promise.resolve();
    changed({ payload: false }); resolve(true);
    await Promise.resolve();
    expect(focus.hidden()).toBe(true);
    off();
  });

  it("keeps motion paused until both document visibility and focus allow it", () => {
    const documentState = source();
    const focus = source();
    const combined = anyHidden(documentState.visibility, focus.visibility);
    expect(combined.hidden()).toBe(false);
    focus.setHidden(true);
    expect(combined.hidden()).toBe(true);
    documentState.setHidden(true);
    focus.setHidden(false);
    expect(combined.hidden()).toBe(true);
    documentState.setHidden(false);
    expect(combined.hidden()).toBe(false);
  });

  it("reports only combined transitions and unsubscribes from both", () => {
    const documentState = source();
    const focus = source();
    const combined = anyHidden(documentState.visibility, focus.visibility);
    let calls = 0;
    const stop = combined.onChange(() => { calls++; });
    documentState.setHidden(true);
    focus.setHidden(true);
    documentState.setHidden(false);
    expect(calls).toBe(1);
    focus.setHidden(false);
    expect(calls).toBe(2);
    expect(documentState.listeners.size).toBe(1);
    expect(focus.listeners.size).toBe(1);
    stop();
    expect(documentState.listeners.size).toBe(0);
    expect(focus.listeners.size).toBe(0);
    documentState.setHidden(true);
    focus.setHidden(true);
    expect(calls).toBe(2);
  });
});
