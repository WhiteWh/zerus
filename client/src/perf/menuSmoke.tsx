// Native WebKit component regression. Served by Vite, outside the app entry.
import { render } from "solid-js/web";
import { MenuLayer } from "../ui/components/Overlays";
import { sessionContextMenu } from "../ui/sessions/SessionList";
import { applyBatch, toSession } from "../store/catalog";
import { bindMenuBarrier, menu, setMenu } from "../store/ui";
import { emptyBatch } from "../types";
import { applySkin, skins } from "../skin/runtime";
import "../ui/styles/base.css";
import "../ui/styles/app.css";
import "../ui/styles/motion.css";

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
const root = document.getElementById("root")!;
const dispose = render(() => <MenuLayer />, root);
document.documentElement.dataset.platform = "linux";
const failures: string[] = [], cases: unknown[] = [];
const check = (condition: unknown, message: string) => { if (!condition) failures.push(message); };
const popup = () => root.querySelector<HTMLElement>(".menu");
const painted = () => { const el = popup(); return el && getComputedStyle(el).visibility !== "hidden" && getComputedStyle(el).opacity !== "0" ? el : undefined; };
const buttons = () => [...root.querySelectorAll<HTMLButtonElement>(".menu-item")];
const deferred = <T,>() => { let resolve!: (value: T) => void; const promise = new Promise<T>((done) => { resolve = done; }); return { promise, resolve }; };

async function sample(name: string, open: () => void, replies?: () => Promise<void>, maxPaintMs?: number) {
  setMenu(undefined);
  await frame();
  let first: HTMLElement | undefined, initial: Element[] = [], moves = 0, replacements = 0, mounts = 0, removals = 0, samples = 0;
  let box: DOMRect | undefined, firstPaintMs: number | undefined, firstMeasurements = 0, measurements = 0;
  // Count application layout reads separately from the frame sampler's rects.
  const dimensions = ["offsetWidth", "offsetHeight"].map((name) => ({ name, descriptor: Object.getOwnPropertyDescriptor(HTMLElement.prototype, name)! }));
  for (const { name, descriptor } of dimensions) Object.defineProperty(HTMLElement.prototype, name, { ...descriptor, get() {
    if (this.classList.contains("menu")) measurements++;
    return descriptor.get!.call(this);
  } });
  const started = performance.now();
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      for (const n of record.addedNodes) if (n instanceof Element) { if (n.matches(".menu")) mounts++; }
      for (const n of record.removedNodes) if (n instanceof Element) {
        if (n.matches(".menu-item") || n.querySelector(".menu-item")) removals++;
      }
    }
  });
  observer.observe(root, { childList: true, subtree: true });
  let running = true;
  const tick = () => {
    const el = painted();
    if (el) {
      const r = el.getBoundingClientRect(), current = buttons();
      if (!first) { first = el; initial = current; box = r; firstPaintMs = performance.now() - started; firstMeasurements = measurements; }
      else {
        if (el !== first || current.some((button, i) => button !== initial[i]) || current.length !== initial.length) replacements++;
        if (Math.abs(r.left - box!.left) > .1 || Math.abs(r.top - box!.top) > .1 || Math.abs(r.width - box!.width) > .1 || Math.abs(r.height - box!.height) > .1) moves++;
      }
      samples++;
    }
    if (running) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  open();
  await replies?.();
  await sleep(400);
  running = false; observer.disconnect();
  for (const { name, descriptor } of dimensions) Object.defineProperty(HTMLElement.prototype, name, descriptor);
  check(samples >= 3, `${name}: no painted frames`);
  check(mounts === 1 && removals === 0 && replacements === 0, `${name}: mounts=${mounts}, removals=${removals}, replacements=${replacements}`);
  check(moves === 0, `${name}: ${moves} frames changed position or size after first paint`);
  check(measurements === firstMeasurements, `${name}: ${measurements - firstMeasurements} layout reads after first paint`);
  if (maxPaintMs !== undefined) check(firstPaintMs !== undefined && firstPaintMs <= maxPaintMs, `${name}: first paint ${firstPaintMs ?? "missing"} ms exceeds ${maxPaintMs} ms`);
  if (box) check(box.left >= 7.9 && box.top >= 7.9 && box.right <= innerWidth - 7.9 && box.bottom <= innerHeight - 7.9, `${name}: outside viewport`);
  check(!popup()?.getAnimations().length, `${name}: entrance animation changes painted geometry`);
  cases.push({ name, samples, mounts, removals, replacements, moves, firstPaintMs, lateMeasurements: measurements - firstMeasurements, box: box && { x: box.x, y: box.y, width: box.width, height: box.height } });
}

async function run() {
  await document.fonts.ready;
  // Real session caller, with only the native reads replaced by controlled IPC.
  // No real CLI, account, session, filesystem action or installed app is used.
  const row = { name: "codex/example/menu", agent: "codex", state: "running", status: "ready", run_id: "menu-run", conversation_id: "menu-conversation", cwd: "/example/menu" };
  applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }], upserts: [{ computer: "", key: "run:menu-run", row }] });
  const session = toSession("", "run:menu-run", row);
  const folder = deferred<unknown>(), inspect = deferred<unknown>(), clear = deferred<unknown>();
  const native = window as unknown as { __TAURI_INTERNALS__?: { invoke: (command: string) => Promise<unknown> } };
  native.__TAURI_INTERNALS__ = { invoke: (command) => {
    if (command === "session_folder_status") return folder.promise;
    if (command === "inspect_session") return inspect.promise;
    if (command === "composer_clear_control") return clear.promise;
    return Promise.reject(new Error(`Unexpected native command: ${command}`));
  } };
  applySkin(skins[0]);
  await sample("session async reads", () => sessionContextMenu(new MouseEvent("contextmenu", { clientX: innerWidth - 12, clientY: innerHeight - 12, button: 2 }), session), async () => {
    await sleep(40);
    check(!!menu() && !painted(), "session async reads: painted before final capabilities");
    folder.resolve({ folder: true, shell: true, folder_hint: "" });
    await sleep(40);
    check(!painted(), "session async reads: painted before inspection and Clear finished");
    inspect.resolve({ run_id: row.run_id, conversation_id: row.conversation_id, fork_supported: true });
    clear.resolve({ enabled: true, reason: "", run: row.run_id, conversation: row.conversation_id });
  });
  for (const label of ["Open folder", "Clear session…"]) {
    const button = buttons().find((b) => b.textContent?.trim() === label);
    check(button && button.getAttribute("aria-disabled") !== "true", `${label}: final state missing`);
  }
  const slowRow = { ...row, name: "codex/example/slow-menu", run_id: "slow-menu-run", conversation_id: "slow-menu-conversation" };
  applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:slow-menu-run", row: slowRow }] });
  const slowSession = toSession("", "run:slow-menu-run", slowRow);
  const slowFolder = deferred<unknown>(), slowInspect = deferred<unknown>(), slowClear = deferred<unknown>();
  native.__TAURI_INTERNALS__ = { invoke: (command) => {
    if (command === "session_folder_status") return slowFolder.promise;
    if (command === "inspect_session") return slowInspect.promise;
    if (command === "composer_clear_control") return slowClear.promise;
    return Promise.reject(new Error(`Unexpected native command: ${command}`));
  } };
  await sample("stalled inspection opens within bound", () => sessionContextMenu(new MouseEvent("contextmenu", { clientX: innerWidth - 12, clientY: innerHeight - 12, button: 2 }), slowSession), async () => {
    // All reads are still unresolved when the preparation budget expires.
    await sleep(230);
    check(!!painted(), "stalled inspection: no menu after the preparation bound");
    buttons()[1]?.focus(); const focused = document.activeElement;
    slowFolder.resolve({ folder: true, shell: true, folder_hint: "" });
    slowClear.resolve({ enabled: true, reason: "", run: slowRow.run_id, conversation: slowRow.conversation_id });
    await frame(); await frame();
    for (const label of ["Open folder", "Clear session…"]) {
      const button = buttons().find((b) => b.textContent?.trim() === label);
      check(button && button.getAttribute("aria-disabled") !== "true", `${label}: blocked behind stalled inspection`);
    }
    check(document.activeElement === focused, "partial replies stole keyboard focus");
    const fork = buttons().find((b) => b.textContent?.trim() === "Fork session…"), checking = fork?.title;
    await sleep(80);
    slowInspect.resolve({ run_id: slowRow.run_id, conversation_id: slowRow.conversation_id, fork_supported: true });
    await frame(); await frame();
    check(fork?.title !== checking, "late inspection did not update its existing button");
    check(document.activeElement === focused, "late inspection stole keyboard focus");
  }, 260); // 180 ms budget plus frame/timer scheduling allowance.
  delete native.__TAURI_INTERNALS__;
  // Every other context/dropdown caller shares this exact layer and CSS.
  for (const skin of skins.slice(0, 2)) {
    applySkin(skin);
    for (const [x, y] of [[-20, -20], [innerWidth - 2, innerHeight - 2], [200, 180]]) {
      await sample(`${skin.id} context/dropdown ${x}/${y}`, () => setMenu({ x, y, items: [{ heading: "Choose machines" }, { label: "All machines (12)" }, { separator: true }, { label: "Example computer (4)", checked: true }, { label: "Offline computer (8)", disabled: true }] }));
    }
  }
  // Scope selectors refresh counts/capabilities while open. Their button and
  // keyboard focus must survive the new object snapshots, without reposition.
  await sample("dropdown async update", () => setMenu({ x: 200, y: 180, items: [{ label: "First", disabled: true }, { label: "Second" }] }), async () => {
    await sleep(100);
    buttons()[1].focus(); const focused = document.activeElement;
    const hidden = deferred<void>(); bindMenuBarrier(() => hidden.promise);
    setMenu({ ...menu()!, items: [{ label: "First", disabled: false }, { label: "Second" }] });
    await frame();
    hidden.resolve(); bindMenuBarrier(undefined);
    await frame();
    check(document.activeElement === focused, "dropdown update stole keyboard focus");
  });
  // Escape/outside click must also cancel preparation, not reveal a late menu.
  for (const event of [() => new KeyboardEvent("keydown", { key: "Escape", bubbles: true }), () => new MouseEvent("mousedown", { bubbles: true })]) {
    const hidden = deferred<void>(); bindMenuBarrier(() => hidden.promise);
    setMenu({ x: 100, y: 100, items: [{ label: "Canceled" }] });
    await sleep(30); document.body.dispatchEvent(event()); hidden.resolve(); await sleep(30);
    check(!menu() && !popup(), "dismissed pending menu reappeared");
    bindMenuBarrier(undefined);
  }
  setMenu(undefined); dispose();
  const report = { failures, cases, userAgent: navigator.userAgent };
  document.body.textContent = JSON.stringify(report);
  (window as unknown as { webkit: { messageHandlers: { result: { postMessage: (text: string) => void } } } }).webkit.messageHandlers.result.postMessage(JSON.stringify(report));
}
void run().catch((error) => {
  (window as unknown as { webkit: { messageHandlers: { result: { postMessage: (text: string) => void } } } }).webkit.messageHandlers.result.postMessage(JSON.stringify({ failures: [String(error)] }));
});
