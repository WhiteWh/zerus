import { invoke } from "@tauri-apps/api/core";
import { getAllWebviews } from "@tauri-apps/api/webview";
import { orderedRows } from "../store/catalog";
import { nativeUi } from "../store/native-ui";
import { composer } from "../store/composer";
import { activity } from "../store/session";
import { selected, setDetailTab, setPage, setSearch, setSelectedKey, setStateFilter, setMenu } from "../store/ui";
import { phase4Smoke } from "./phase4Smoke";

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const log = (line: string) => invoke("perf_log", { line: "native-ui-smoke: " + line });
const require = (ok: unknown, reason: string) => { if (!ok) throw Error(reason); };
async function until(test: () => boolean | Promise<boolean>, why: string, timeout = 20_000) {
  const end = performance.now() + timeout;
  while (!await test()) { if (performance.now() > end) throw Error(why); await sleep(30); }
}
async function control(step: string) {
  await until(async () => (await invoke<{ argv: string[] }[]>("smoke_log")).some((call) => call.argv[0] === "native-smoke-control" && call.argv[1] === step), `private driver did not advance ${step}`, 90_000);
}
function button(label: string) {
  const found = [...document.querySelectorAll<HTMLButtonElement>("button")].find((el) => el.textContent?.trim() === label);
  require(found, `missing ${label}`);
  return found!;
}
async function ready(stage: string, expectedChildren = 1) {
  await until(() => ["ready", "failed", "browser"].includes(nativeUi.status().status), "native lookup did not settle");
  require(nativeUi.status().status === "ready", nativeUi.status().text);
  const children = (await getAllWebviews()).filter((view) => view.label.startsWith("native-ui-"));
  require(children.length === expectedChildren && children.length <= 4, "the native profile cache is missing, duplicated or unbounded");
  await invoke("display_hz"); // Main-handle lookup while Tauri classifies this as a multi-webview window.
  await sleep(150);
  const r = document.querySelector<HTMLElement>(".native-frame")!.getBoundingClientRect();
  await log(stage + " " + JSON.stringify({ x: r.x + 1, y: r.y + 1, width: r.width - 2, height: r.height - 2, child: children[0].label }));
}

/** Only the private fake-hgs/replay harness enables this. The child itself
 * reports IPC/protocol results to its own local HTTP fixture, never to Tauri. */
export async function nativeUiSmoke(allowed: boolean, smokeFile?: string) {
  if (!allowed) return;
  try {
    const name = "deepseek/docs-site/examples";
    await until(() => orderedRows().some((row) => row.name === name), "native fixture row did not arrive");
    const row = orderedRows().find((row) => row.name === name)!;
    setPage("sessions"); setStateFilter("all"); setSearch(""); setDetailTab("activity"); setSelectedKey(row.key);
    await until(() => activity.target()?.id === row.key && activity.loaded() && composer.view()?.loaded === true, "native Activity did not load");
    button("Native UI").click();
    const geometry = () => {
      requestAnimationFrame(() => {
        const r = document.querySelector<HTMLElement>(".native-frame")?.getBoundingClientRect();
        if (r) void log("GEOMETRY " + JSON.stringify({ x: r.x + 1, y: r.y + 1, width: r.width - 2, height: r.height - 2 }));
      });
    };
    window.addEventListener("resize", geometry);
    await ready("READY");
    require(document.querySelector(".term-bar b")?.textContent === `DeepSeek workspace / ${row.machine}`, "native toolbar diverged from mockup");
    await control("ready");

    document.querySelector<HTMLButtonElement>('[data-tip="Session actions"]')!.click();
    await until(() => !!document.querySelector(".menu"), "menu did not open over the native child");
    await log("MENU"); await control("menu");
    const rename = [...document.querySelectorAll<HTMLButtonElement>(".menu-item")].find((el) => el.textContent?.startsWith("Rename…"));
    require(rename, "Rename session route is absent"); rename!.click();
    await until(() => !!document.querySelector("dialog[open]"), "rename dialog did not open");
    await log("DIALOG"); await control("dialog");
    const dialog = document.querySelector("dialog[open]")!;
    const cancel = [...dialog.querySelectorAll<HTMLButtonElement>("button")].find((el) => el.textContent === "Cancel");
    require(cancel, "rename has no Cancel"); cancel!.click();
    await until(() => !document.querySelector("dialog[open]"), "rename did not close");
    setMenu(undefined);
    await ready("RESTORED"); await control("restored");
    nativeUi.reload();
    await ready("RELOADED"); await control("reloaded");
    for (let index = 1; index <= 5; index++) {
      await log(`PROFILE_CHANGE ${index}`); await control(`profile-change-${index}`);
      nativeUi.reload();
      await ready(`PROFILE_READY ${index}`, Math.min(index + 1, 4)); await control(`profile-ready-${index}`);
    }
    const request = nativeUi.request()!;
    nativeUi.apply(await invoke("native_ui_open", { request, reload: false, embedded: false }));
    await until(() => nativeUi.status().status === "browser", "unsupported embedding did not offer browser fallback");
    require(document.querySelector(".native-frame .empty p")?.textContent === nativeUi.status().text, "fallback did not use the mockup empty state");
    await log("FALLBACK"); await control("fallback");
    const browserCalls = async () => (await invoke<{ argv: string[] }[]>("smoke_log"))
      .filter((call) => call.argv[0] === "native-ui" && !call.argv.includes("--json"));
    require((await browserCalls()).length === 0, "browser fallback opened without a menu click");
    document.querySelector<HTMLButtonElement>('[data-tip="Session actions"]')!.click();
    await until(() => !!document.querySelector(".menu"), "native menu did not open for fallback");
    button("Open native DeepSeek UI").click();
    await until(async () => (await browserCalls()).length === 1, "explicit native menu did not open browser fallback");
    require((await browserCalls())[0].argv[1] === row.name, "fallback lost the original session");
    nativeUi.reload();
    await ready("FINAL_READY", 4); await control("final-ready");
    window.removeEventListener("resize", geometry);
    require(selected()?.runId === row.runId && selected()?.conversationId === row.conversationId, "native route changed the original target");
    await log("PASS child, original identity, native main lookup, menu/dialog hide and restore; continuing full Phase4 with child retained");
    await phase4Smoke(allowed, smokeFile);
  } catch (error) {
    await log("FAIL " + String(error));
    await invoke("exit_app", { code: 1 });
  }
}
