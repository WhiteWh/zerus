import { createSignal } from "solid-js";
import type { Target } from "../types";
import { trNoop, tr } from "../i18n";

export interface NativeBinding { catalog_key: string; target: Target }
export interface NativeRequest extends NativeBinding { nonce: string }
export type NativePhase = "idle" | "connecting" | "loading" | "ready" | "browser" | "failed" | "stopped" | "offline";
export interface NativeStatus { request: NativeRequest; revision: number; status: NativePhase; text: string }
export interface NativeBounds { nonce: string; x: number; y: number; width: number; height: number }
export interface NativeCalls {
  open(request: NativeRequest, reload: boolean): Promise<NativeStatus>;
  retire(): Promise<void>;
  visible(nonce: string, visible: boolean): Promise<void>;
  bounds(bounds: NativeBounds): Promise<void>;
  browser(request: NativeRequest): Promise<void>;
}

export const NATIVE_OFFLINE = "Native UI is unavailable while this machine is offline.";
export const NATIVE_OPEN: string = trNoop("Open the native workspace to manage DeepSeek sessions and settings.");
export const NATIVE_CONNECTING: string = trNoop("Connecting to native DeepSeek UI…");
export const NATIVE_FAILED = "Native UI could not load. Use Reload to reconnect, or open it in your browser.";

const identity = (binding: NativeBinding | undefined) => binding && JSON.stringify([
  binding.catalog_key, binding.target.computer, binding.target.name, binding.target.run_id, binding.target.conversation_id,
]);

/** One selected binding, one in-flight lookup and one coalesced next request.
 * Native profiles live in Rust's bounded cache, never a per-session JS map. */
export function createNativeUi(calls: NativeCalls, fresh: () => string = () => crypto.randomUUID()) {
  const [status, setStatus] = createSignal<{ status: NativePhase; text: string }>({ status: "idle", text: tr(NATIVE_OPEN) });
  const [request, setRequest] = createSignal<NativeRequest>();
  let binding: NativeBinding | undefined, signature: string | undefined;
  let current: NativeRequest | undefined, online = false, active = false, suspended = false;
  let revision = 0, phase: NativePhase = "idle", pending = false, needed = false, reload = false, started = false;
  let retiring: Promise<void> = Promise.resolve();
  let rectangle: Omit<NativeBounds, "nonce"> | undefined;
  let browserIntent: string | undefined;
  let currentText = tr(NATIVE_OPEN);
  let placement: Promise<void> | undefined, placementDirty = false;
  let retirePending = false, retireFailed = false;
  let browserOpening = false;

  const update = (next: NativePhase, text: string) => { if (phase === next && currentText === text) return; phase = next; currentText = text; setStatus({ status: next, text }); };
  // Foreground intent stays true during a navigation. Rust shows only a
  // Ready child, but a download response may arrive after Loading and must
  // still be allowed to ask for Save for this exact foreground workspace.
  const shown = () => !!current && !!rectangle && online && active && !suspended;
  // One placement exchange at a time. A suspension waits for any earlier
  // show to finish, then acknowledges the hide before an overlay can open.
  const layout = (): Promise<void> => {
    placementDirty = true;
    if (placement) return placement;
    placement = Promise.resolve().then(async () => {
      try {
      await retiring;
      while (placementDirty) {
        placementDirty = false;
        const mine = current, rect = rectangle;
        if (!mine || !started) continue;
        if (rect) await calls.bounds({ ...rect, nonce: mine.nonce });
        await calls.visible(mine.nonce, current?.nonce === mine.nonce && shown());
      }
      } finally { placement = undefined; }
    });
    return placement;
  };
  const retire = () => {
    if (started || retireFailed) {
      started = false;
      retirePending = true; retireFailed = false;
      retiring = calls.retire().then(() => { retirePending = false; }, (error: unknown) => {
        retirePending = false; retireFailed = true; throw error;
      });
    }
    // A failure stays attached to the next lookup. It cannot silently grant
    // a new child while the old one may still be visible.
    void retiring.catch(() => undefined);
  };
  const replace = () => {
    retire();
    rectangle = undefined;
    revision = 0;
    current = binding ? { catalog_key: binding.catalog_key, target: { ...binding.target }, nonce: fresh() } : undefined;
    setRequest(current);
    needed = !!current && online;
    browserIntent = undefined;
    update(!binding || online ? "idle" : "offline", tr(!binding || online ? NATIVE_OPEN : NATIVE_OFFLINE));
  };
  const browser = async () => {
    const mine = current;
    if (!mine || !online || browserOpening) return;
    browserOpening = true;
    try { await calls.browser(mine); } finally { browserOpening = false; void wake(); }
  };
  const maybeBrowser = () => {
    if (pending || !current || !online || phase !== "browser" || browserIntent !== current.nonce) return;
    const nonce = current.nonce;
    browserIntent = undefined;
    void browser().catch(() => { if (current?.nonce === nonce) update("failed", NATIVE_FAILED); });
  };
  const apply = (value: NativeStatus) => {
    if (!current || value.request.nonce !== current.nonce || identity(value.request) !== identity(current) || value.revision <= revision) return;
    revision = value.revision;
    update(value.status, value.text);
    void layout().catch(() => undefined);
    // Only an explicit session-menu click asks for browser fallback. A late
    // reply, tab mount or reload cannot manufacture another browser action.
    maybeBrowser();
  };
  const wake = async () => {
    if (pending || browserOpening || !needed || !active || !online || !current) return;
    pending = true;
    const mine = current, force = reload;
    needed = false; reload = false;
    try {
      await retiring;
      if (current?.nonce !== mine.nonce || !active || !online) {
        if (current?.nonce === mine.nonce && online) needed = true;
        return;
      }
      started = true;
      update("connecting", NATIVE_CONNECTING);
      apply(await calls.open(mine, force));
    } catch {
      if (current?.nonce === mine.nonce) update("failed", NATIVE_FAILED);
    } finally {
      pending = false;
      maybeBrowser();
      void wake();
    }
  };
  return {
    status, request, apply,
    barrierNeeded: () => retirePending || retireFailed || (started && ((active && !suspended) || !!placement)),
    select(next: NativeBinding | undefined, available: boolean) {
      const nextSignature = identity(next);
      if (nextSignature === signature && available === online) return;
      signature = nextSignature; binding = next; online = available;
      replace();
      void wake();
    },
    opened(value: boolean) {
      if (active === value) return;
      active = value;
      if (value) rectangle = undefined;
      if (value && ["failed", "stopped"].includes(phase)) replace();
      void layout().catch(() => undefined);
      void wake();
    },
    async suspend(value: boolean) {
      suspended = value;
      await layout();
    },
    rectangle(value: Omit<NativeBounds, "nonce">) {
      rectangle = Object.values(value).every(Number.isFinite) && value.width > 0 && value.height > 0 ? value : undefined;
      void layout().catch(() => undefined);
    },
    reload() {
      if (!binding || !online) return;
      replace(); reload = true;
      void wake();
    },
    fromMenu() {
      if (!current || !online || browserOpening) return;
      browserIntent = current.nonce;
      maybeBrowser();
      void wake();
    },
  };
}
