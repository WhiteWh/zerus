import { bindMenuBarrier } from "./ui";

/** Fail closed on engines without cancellable dialog beforetoggle. No native
 * child is shown there; the existing browser route remains available. */
export function installNativeOverlays(port: { suspend(value: boolean): Promise<void>; barrierNeeded(): boolean }): boolean {
  if (typeof HTMLDialogElement === "undefined" || typeof document === "undefined") return false;
  const probe = document.createElement("dialog");
  probe.style.display = "none";
  let supported = false;
  probe.addEventListener("beforetoggle", (event) => { supported = event.cancelable; event.preventDefault(); });
  document.body.append(probe);
  try { probe.showModal(); supported &&= !probe.open; } catch { supported = false; }
  if (probe.open) probe.close();
  probe.remove();
  if (!supported) return false;

  const overlays = new Set<string>();
  const pending = new Set<HTMLDialogElement>();
  const permitted = new WeakSet<HTMLDialogElement>();
  const blocked = () => overlays.size > 0 || pending.size > 0 || !!document.querySelector("dialog[open]");
  const restore = () => { void port.suspend(blocked()).catch(() => {}); };
  bindMenuBarrier((scope, opening) => {
    if (opening) overlays.add(scope); else overlays.delete(scope);
    const needed = port.barrierNeeded();
    const wait = port.suspend(blocked());
    if (needed) return wait;
    void wait.catch(() => {});
  });
  document.addEventListener("beforetoggle", (event) => {
    const dialog = event.target;
    if (!(dialog instanceof HTMLDialogElement)) return;
    if ((event as ToggleEvent).newState !== "open") { queueMicrotask(restore); return; }
    if (permitted.delete(dialog)) return;
    if (!port.barrierNeeded()) { void port.suspend(true).catch(() => {}); return; }
    // Delay only when a hide needs an asynchronous acknowledgement. Normal
    // dialogs still receive their native focus after the barrier completes.
    event.preventDefault();
    if (pending.has(dialog)) return;
    pending.add(dialog);
    void port.suspend(true).then(() => {
      pending.delete(dialog);
      if (!dialog.isConnected || dialog.open) { restore(); return; }
      permitted.add(dialog);
      dialog.showModal();
    }).catch(() => { pending.delete(dialog); });
  }, true);
  document.addEventListener("close", restore, true);
  // Dialog components may unmount without calling close. Only removals can
  // release this barrier; Activity's frequent text additions do no work.
  new MutationObserver((records) => {
    if (records.some((record) => [...record.removedNodes].some((node) => node instanceof Element
      && (node.matches("dialog") || !!node.querySelector("dialog"))))) restore();
  }).observe(document.body, { subtree: true, childList: true });
  return true;
}
