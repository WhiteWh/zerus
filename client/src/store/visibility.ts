import { createSignal } from "solid-js";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

// The webview derives the page's visibility from its window. The batch pump
// (bridge.ts) and Working clocks (store/ui.ts) use only that document state;
// Linux endless motion also pauses on toplevel focus loss because a Wayland
// window can keep a visible document while minimised and the GL path paints it.

export interface PageVisibility {
  hidden(): boolean;
  /** Calls `listener` after every change; returns the unsubscribe. */
  onChange(listener: () => void): () => void;
}

export const documentVisibility: PageVisibility = {
  hidden: () => typeof document !== "undefined" && document.visibilityState === "hidden",
  onChange(listener) {
    if (typeof document === "undefined") return () => {};
    document.addEventListener("visibilitychange", listener);
    return () => document.removeEventListener("visibilitychange", listener);
  },
};

/** Hidden when any source is hidden; listeners fire only when the combined state changes. */
export function anyHidden(...sources: PageVisibility[]): PageVisibility {
  const hidden = () => sources.some((source) => source.hidden());
  return {
    hidden,
    onChange(listener) {
      let last = hidden();
      const changed = () => {
        const current = hidden();
        if (current === last) return;
        last = current;
        listener();
      };
      const stops = sources.map((source) => source.onChange(changed));
      return () => { for (const stop of stops) stop(); };
    },
  };
}

export interface NativeFocus {
  isFocused(): Promise<boolean>;
  onFocusChanged(listener: (event: { payload: boolean }) => void): Promise<() => void>;
}

/** Toplevel focus remains true while a native child, such as DeepSeek's GTK overlay, has focus. */
export function nativeFocusVisibility(native: NativeFocus): PageVisibility {
  let focused = true, epoch = 0, revision = 0, stop: (() => void) | undefined;
  const listeners = new Set<() => void>();
  const update = (value: boolean) => {
    revision++;
    if (focused === value) return;
    focused = value;
    for (const listener of [...listeners]) listener();
  };
  return {
    hidden: () => !focused,
    onChange(listener) {
      listeners.add(listener);
      if (listeners.size === 1) {
        const generation = ++epoch;
        void native.onFocusChanged(({ payload }) => { if (generation === epoch) update(payload); }).then((unlisten) => {
          if (generation !== epoch) { unlisten(); return; }
          stop = unlisten;
          const readRevision = revision;
          void native.isFocused().then((value) => {
            if (generation === epoch && revision === readRevision) update(value);
          }).catch(() => {});
        }).catch(() => {});
      }
      return () => {
        listeners.delete(listener);
        if (!listeners.size) { epoch++; stop?.(); stop = undefined; }
      };
    },
  };
}

export function focusMotionVisibility(document: PageVisibility, focus: PageVisibility, linux: boolean): PageVisibility {
  return linux ? anyHidden(document, focus) : document;
}

const linux = typeof navigator !== "undefined" && /Linux/.test(navigator.userAgent);
const windowFocus = linux && isTauri() ? nativeFocusVisibility(getCurrentWindow())
  : { hidden: () => false, onChange: () => () => {} };
const [unfocused, setUnfocused] = createSignal(windowFocus.hidden());
windowFocus.onChange(() => setUnfocused(windowFocus.hidden()));
export const motionUnfocused = unfocused;

export const motionVisibility = focusMotionVisibility(documentVisibility, windowFocus, linux);
const [paused, setPaused] = createSignal(motionVisibility.hidden());
motionVisibility.onChange(() => setPaused(motionVisibility.hidden()));

/** Reactive: pause endless motion when hidden, or when the Linux toplevel is unfocused. */
export const motionPaused = paused;

// Data subscriptions, selection and media still use document visibility only.
const [hidden, setHidden] = createSignal(documentVisibility.hidden());
documentVisibility.onChange(() => setHidden(documentVisibility.hidden()));

/** Reactive: true while the document is hidden. */
export const pageHidden = hidden;

/**
 * Whether the window is the active one (Qt's isActiveWindow): the webview's
 * document has the focus. Not reactive; window `focus` and `blur` say when it
 * changes.
 */
export const documentFocused = () => typeof document !== "undefined" && document.hasFocus();
