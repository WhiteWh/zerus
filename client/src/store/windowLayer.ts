import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { setSetting } from "./settings";
import { bus } from "./notices";
import { trNoop, tr } from "../i18n";

export const LAYER_UNAVAILABLE = trNoop("Keep Above is unavailable in this Zerus session. On Wayland, KDE Plasma 6 with KWin scripting is required. You can use your desktop's window menu or rules.");
export interface WindowLayer { supported: boolean; on: boolean; busy: boolean; hint: string; revision: number }
export interface LayerPort {
  load(): Promise<WindowLayer>;
  listen(receive: (view: WindowLayer) => void): Promise<() => void>;
  save(on: boolean): Promise<boolean>;
  error(text: string): void;
}

/** Native confirmation drives both controls, including KDE window-menu changes. */
export function createWindowLayer(port: LayerPort) {
  const [view, setView] = createSignal<WindowLayer>({ supported: false, on: false, busy: false, hint: LAYER_UNAVAILABLE, revision: -1 });
  let pending = false;
  let stop: (() => void) | undefined;
  const receive = (next: WindowLayer) => { if (next.revision > view().revision) setView(next); };
  const ready = (async () => {
    try { stop = await port.listen(receive); receive(await port.load()); }
    catch { /* The unavailable hint stays visible. */ }
  })();
  async function set(on: boolean) {
    // Keep the focused controls enabled during a request, as Qt does.
    if (pending || view().busy || !view().supported) return;
    pending = true;
    try { if (!await port.save(on)) { try { receive(await port.load()); } catch {} port.error(view().hint); } }
    catch { port.error(view().hint); }
    finally { pending = false; }
  }
  return { view, set, ready, dispose: () => stop?.() };
}

const native = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const layer = createWindowLayer({
  load: () => native ? invoke<WindowLayer>("window_layer_state") : Promise.reject(),
  listen: (receive) => native ? listen<WindowLayer>("window-layer-changed", (event) => receive(event.payload)) : Promise.resolve(() => {}),
  save: (on) => setSetting("workspace/alwaysOnTop", on),
  error: (text) => bus.notify("error", text, true, "window-layer", tr("Keep Zerus above other windows")),
});
export const windowLayer = layer.view;
export const setWindowLayer = layer.set;
