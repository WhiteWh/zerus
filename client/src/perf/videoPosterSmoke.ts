/** Private source JPEG/MP4 qualification; requires replay and explicit fake hgs. */
import { invoke } from "@tauri-apps/api/core";
import { catalog } from "../store/catalog";
import { activity } from "../store/session";
import { setDetailTab, setPage, setSelectedKey } from "../store/ui";
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(test: () => boolean, why: string) {
  const end = performance.now() + 20_000;
  while (!test()) { if (performance.now() > end) throw Error(why); await sleep(20); }
}
function require(test: boolean, why: string) { if (!test) throw Error(why); }
export async function videoPosterSmoke(allowed: boolean) {
  if (!allowed) return;
  const log = (line: string) => invoke("perf_log", { line: "video-poster-smoke: " + line });
  const original = window.fetch.bind(window);
  const reads = new Map<string, number>();
  const aborted = new Set<string>();
  try {
    await until(() => Object.values(catalog.rows).some((row) => row.catalogKey === "run:video-one"), "synthetic catalog missing");
    setPage("settings"); await sleep(200);
    await log("ORIGIN " + location.origin);
    window.fetch = async (input, options) => {
      const address = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      const uri = new URL(address);
      if (uri.pathname === "/poster") {
        const message = uri.searchParams.get("message_id")!;
        reads.set(message, (reads.get(message) ?? 0) + 1);
        const signal = options?.signal;
        signal?.addEventListener("abort", () => { aborted.add(message); void log("ABORT " + message); }, { once: true });
        if (message === "message-20" || message === "message-1") await new Promise<void>((resolve, reject) => {
          const timer = setTimeout(resolve, message === "message-20" ? 1500 : 2000);
          const cancel = () => { clearTimeout(timer); reject(new DOMException("cancelled", "AbortError")); };
          if (signal?.aborted) cancel(); else signal?.addEventListener("abort", cancel, { once: true });
        });
      }
      try {
        const response = await original(input, options);
        if (uri.pathname === "/poster") await log("FETCH " + JSON.stringify({ message: uri.searchParams.get("message_id"), status: response.status, type: response.headers.get("content-type") }));
        return response;
      } catch (error) { if (uri.pathname === "/poster") await log("FETCH_ERROR " + String(error)); throw error; }
    };
    const row = Object.values(catalog.rows).find((row) => row.catalogKey === "run:video-one")!;
    setPage("sessions"); setSelectedKey(row.key); setDetailTab("activity");
    await until(() => document.querySelectorAll(".vm-poster").length === 20 && reads.has("message-20"), "visible poster did not request bytes");
    const cards = () => [...document.querySelectorAll<HTMLButtonElement>(".vm-poster")];
    const last = cards().at(-1)!;
    const scroller = document.querySelector<HTMLElement>(".act-scroll")!;
    const geometry = () => { const rect = last.getBoundingClientRect(); return [rect.width, rect.height, rect.top, scroller.scrollTop, scroller.scrollHeight]; };
    const before = geometry();
    require(!last.querySelector("img"), "poster appeared before the delayed read finished");
    require(!reads.has("message-1"), "offscreen initial card fetched before placement");
    require(!JSON.stringify(activity.events()).includes("poster_base64"), "poster crossed the Activity JSON boundary");
    require(!document.querySelector(".vm-frames video"), "video started without an explicit play");
    await log("PLACEHOLDER"); await sleep(250);
    await until(() => !!last.querySelector<HTMLImageElement>("img")?.naturalWidth, "source JPEG did not load");
    const after = geometry();
    require(before.every((n, i) => Math.abs(n - after[i]) <= 1), "late poster moved the Activity geometry");
    require(before[0] === 200 && before[1] === 200, "approved circle dimensions changed");
    await log("POSTER"); await sleep(400);
    last.click();
    await until(() => last.classList.contains("playing"), "private MP4 did not play");
    await sleep(150); last.click();
    await until(() => !last.classList.contains("playing") && !!last.querySelector("img"), "pause did not restore poster");
    await log("PAUSED"); await sleep(300);
    last.click();
    await until(() => last.classList.contains("playing"), "private MP4 did not resume");
    await until(() => !last.classList.contains("playing") && !!last.querySelector("img"), "end did not restore poster");
    await log("ENDED"); await sleep(300);
    const scroll = (top: number) => {
      scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: top ? 1 : -1, bubbles: true }));
      scroller.scrollTop = top;
    };
    scroll(0);
    await until(() => reads.has("message-1"), "newly visible card did not request a poster");
    scroll(scroller.scrollHeight);
    await until(() => aborted.has("message-1"), "leaving the viewport did not cancel the poster read");
    scroll(0); await sleep(500);
    require(reads.get("message-1") === 1 && !cards()[0].querySelector("img"), "cancelled card retried or adopted late bytes");
    await log("TOP"); await sleep(300);
    require([...reads.values()].every((n) => n === 1), "a mounted card made a duplicate read");
    await log("COUNTS " + JSON.stringify({ mounted: cards().length, requests: reads.size, cancelled: aborted.has("message-1"), circle: before.slice(0, 2) }));
    await log("PASS"); await invoke("exit_app", { code: 0 });
  } catch (error) { await log("FAIL " + String(error)); await invoke("exit_app", { code: 1 }); }
  finally { window.fetch = original; }
}
