import { invoke } from "@tauri-apps/api/core";
import { updates } from "../store/updates";
import { page, setPage } from "../store/ui";
import { applyBatch, catalog } from "../store/catalog";

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function until(ready: () => boolean, what: string): Promise<void> {
  const started = performance.now();
  while (!ready()) {
    if (performance.now() - started > 15_000) throw new Error(what);
    await sleep(16);
  }
}

function log(line: string) {
  void invoke("perf_log", { line }).catch(() => {});
}

function require(ok: boolean, what: string) {
  if (!ok) throw new Error(what);
}

async function statusbarSmoke() {
  log("statusbar start");
  const batch = (status: string, attempt = 0) => applyBatch({
    upserts: [], removes: [], replaces: [], activity: [],
    computers: [{ computer: "build-02", status, attempt }],
  });
  applyBatch({
    upserts: [], removes: [], replaces: [], activity: [],
    computers: [
      { computer: "", status: "live", attempt: 0 },
      { computer: "build-01", status: "live", attempt: 0 },
      { computer: "build-02", status: "live", attempt: 0 },
      { computer: "win-lab", status: "live", attempt: 0 },
      { computer: "mac", status: "live", attempt: 0 },
    ],
  });
  setPage("settings");
  await until(() => !!document.querySelector(".updates-card"), "Updates did not render for the connection-state checks");
  require(Object.keys(catalog.computers).length === 5, "the status fixture has extra computers");
  await until(() => document.querySelector<HTMLElement>(".statusbar .conn-ok")?.innerText.replace(/\s+/g, " ").trim() === "5 of 5 machines connected", "the all-connected summary did not render");
  require(!document.querySelector<HTMLElement>(".statusbar .conn-ok")?.textContent?.includes("offline"), "all-connected has an offline tail");

  batch("reconnecting", 2);
  await until(() => document.querySelector(".statusbar .danger")?.textContent?.includes("build-02: reconnecting (attempt 2)") ?? false, `inline connection details did not render: ${JSON.stringify(catalog.computers)} ${document.querySelector(".statusbar")?.textContent}`);
  log("statusbar inline error shown");
  const retry = [...document.querySelectorAll<HTMLButtonElement>(".statusbar button")].find((button) => button.textContent === "Retry now");
  require(!!retry, "Retry now is missing in the connection-error state");
  retry!.click();
  log("statusbar retry clicked");

  batch("probing");
  log("statusbar probing applied");
  log(`calm text: ${JSON.stringify(document.querySelector<HTMLElement>(".statusbar .conn-ok")?.innerText)}`);
  await until(() => document.querySelector<HTMLElement>(".statusbar .conn-ok")?.innerText.replace(/\s+/g, " ").trim() === "4 of 5 machines connected, build-02 offline", "the calm offline-name summary did not render");
  require(![...document.querySelectorAll<HTMLButtonElement>(".statusbar button")].some((button) => button.textContent === "Retry now"), "Retry now shows without a connection error");
  log("statusbar calm checks passed");

  const gap = document.querySelector(".statusbar .grow")!;
  for (const selector of [".status-update", ".status-restart", ".statusbar .warn-chip"]) {
    const button = document.querySelector<HTMLButtonElement>(selector)!;
    require(!!button, `missing status chip ${selector}`);
    require(button.classList.contains("chip-accent"), `${selector} is not an accent chip`);
    require(!button.querySelector(".zlogo"), `${selector} still has the spinning logo`);
    require((button.compareDocumentPosition(gap) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0, `${selector} is not left of the gap`);
  }
  log("statusbar chips passed");
  const fix = document.querySelector<HTMLButtonElement>(".statusbar .warn-chip")!;
  require(fix.textContent === "Fix login item" && fix.title === "macOS limits background priority for this login item", "the macOS login-item chip is wrong");
  fix.click();
  log("statusbar fix clicked");
  await until(() => page() === "settings" && !!document.querySelector(".upd-login"), "the login-item chip did not open Updates");
  log("statusbar Updates notice shown");
  const notice = document.querySelector(".upd-login")!;
  require(notice.textContent?.includes("macOS starts Zerus at background priority, so it can stop responding while the computer is busy.") === true, "the Updates login-item notice is missing");
  [...notice.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "Fix login item")!.click();
  await until(() => (document.getElementById("login-item-dialog") as HTMLDialogElement | null)?.open === true, "login-item confirmation did not open");
  log("statusbar dialog open");
  const dialog = document.getElementById("login-item-dialog") as HTMLDialogElement;
  require(dialog.style.width === "520px", "login-item dialog is not 520 px wide");
  require(dialog.querySelector("p b")?.textContent === "macOS runs Zerus at background priority.", "login-item dialog's first line is not bold");
  const muted = dialog.querySelectorAll("p.muted");
  require(muted.length === 2 && muted[1].innerHTML.includes("<br"), "login-item dialog's muted paragraphs are wrong");
  const footer = [...dialog.querySelectorAll<HTMLButtonElement>(".dlg-foot button")].map((button) => button.textContent);
  require(JSON.stringify(footer) === JSON.stringify(["Cancel", "Update and Restart", "Update Only"]), "login-item footer order is wrong");
  await until(() => document.activeElement?.textContent === "Update Only", "Update Only is not focused");
  [...dialog.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "Cancel")!.click();
  await until(() => !dialog.open, "login-item confirmation did not close");
  log("statusbar dialog closed");
  setPage("sessions");
  await until(() => !!document.querySelector(".vrow .rowhit"), "Sessions did not return after the login-item dialog");
  log("statusbar Sessions returned");
  document.querySelector<HTMLElement>(".vrow .rowhit")!.click();
  await until(() => !!document.querySelector(".usage-refresh"), "the selected session's usage refresh did not render");
  log("statusbar usage shown");

  const refresh = document.querySelector<HTMLButtonElement>(".usage-refresh")!;
  require(refresh.title === "Refresh usage" && refresh.getAttribute("aria-label") === "Refresh usage", "usage refresh title is wrong");
  const icon = refresh.querySelector("svg")!;
  require(icon.getAttribute("width") === "12" && icon.getAttribute("height") === "12", "usage refresh is not 12 px");
  require(document.querySelector(".rail-new")?.getAttribute("data-tip") === "New session (Ctrl+N)", "New session shortcut is missing");
  require(document.querySelector('[aria-label="Sessions"]')?.getAttribute("data-tip") === "Sessions (Ctrl+2)", "Sessions shortcut is missing");
  const tips = [...document.querySelectorAll(".rail [data-tip]")].map((el) => el.getAttribute("data-tip"));
  require(tips.includes("Settings (Ctrl+,)"), "Settings shortcut is missing");
  require(tips.some((tip) => tip?.startsWith("Home — Zerus Next ") && tip.endsWith(" (Ctrl+1)")), "brand tooltip is missing its shortcut");
  const version = document.querySelector<HTMLButtonElement>(".rail-version")!;
  require(version.getAttribute("data-tip")?.endsWith(". Open About") === true, "version tooltip does not open About");
  log("statusbar Rail passed");
  version.click();
  await until(() => page() === "settings" && !!document.querySelector("#set-nav-about"), "the version button did not open About");
  log("statusbar About shown");
}

export async function updatesSmoke(allowed: boolean) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  const state = new URLSearchParams(location.search).get("updates-smoke") ?? "";
  if (!allowed) {
    log("updates-smoke not started: it needs ZERUS_CLIENT_REPLAY and ZERUS_CLIENT_PERF");
    return;
  }
  try {
    log(`updates-smoke location: ${location.href}`);
    await until(() => {
      const view = updates();
      if (state === "unchecked") return view.heading.source === "Updates have not been checked" && !view.release_enabled;
      if (state === "checking") return view.busy && view.heading.source === "Checking for updates…";
      if (state === "available") return view.available && !view.restart_required;
      if (state === "cached-error") return view.cached && !!view.error;
      if (state === "restart-required") return view.restart_required;
      if (state === "statusbar") return view.available && view.restart_required && view.login_item_deprioritized;
      return false;
    }, `update state ${state} did not arrive: ${JSON.stringify(updates())}`);
    if (state !== "statusbar") await until(() => !!document.querySelector(".updates-card"), "Updates did not render");
    const text = () => document.querySelector(".set-content")?.textContent ?? "";
    const updateButton = () => document.querySelector<HTMLButtonElement>(".status-update");
    const restartButton = () => document.querySelector<HTMLButtonElement>(".status-restart");
    const checkButton = () => [...document.querySelectorAll<HTMLButtonElement>(".set-content button")].find((button) => button.textContent === "Check now");

    if (state === "statusbar") {
      await statusbarSmoke();
    } else if (state === "unchecked") {
      require(text().includes("Not checked"), "unchecked Latest release is missing");
      require(checkButton()?.disabled === false, "unchecked Check now is disabled");
      require(!updateButton() && !restartButton(), "unchecked state shows a workspace update action");
    } else if (state === "checking") {
      require(checkButton()?.disabled === true, "checking Check now is enabled");
      require(!text().includes("A previous comparison"), "checking state shows its old comparison");
    } else if (state === "available") {
      require(text().includes("Update available") && text().includes("View release"), "available card is incomplete");
      require(text().includes("Download dev source"), "conditional download action is missing");
      require(updateButton()?.title === "Zerus 0.47.0 is available. Open Updates.", "New version tooltip differs from Qt");
      updateButton()!.click();
      await sleep(100);
      require(page() === "settings", "New version opened a session instead of Settings");
      const disclosure = [...document.querySelectorAll<HTMLButtonElement>(".set-content button")].find((button) => button.textContent === "Show installation details");
      disclosure!.click();
      await until(() => text().includes("yay -S zerus"), "installation command did not open");
      require(document.querySelector('[aria-label="Copy the update command"]') !== null, "Copy command accessible name is missing");
      [...document.querySelectorAll<HTMLButtonElement>(".set-content button")].find((button) => button.textContent === "Hide installation details")!.click();
      await until(() => !text().includes("yay -S zerus"), "installation details did not close");
    } else if (state === "cached-error") {
      require(text().includes("Showing the last verified catalog."), "cached notice is missing");
      require(text().includes("Check failed. Try again later."), "check error is missing");
    } else if (state === "restart-required") {
      require(!!updateButton() && !!restartButton(), "restart state does not show both workspace actions");
      require((updateButton()!.compareDocumentPosition(restartButton()!) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0, "Restart is not after New version");
      require(restartButton()!.title === "Zerus was updated on disk. Drafts are kept; sessions keep running.", "Restart tooltip differs from Qt");
      const gap = document.querySelector(".statusbar .grow")!;
      for (const button of [updateButton()!, restartButton()!]) {
        require(button.classList.contains("chip-accent"), "update action is not an accent chip");
        require(!button.querySelector(".zlogo"), "update action still has the spinning logo");
        require((button.compareDocumentPosition(gap) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0, "update action is not left of the gap");
      }
    }
    log(`updates-smoke ${state}: ready`);
    await sleep(3000);
    await exit(0);
  } catch (error) {
    log(`updates-smoke ${state}: failed: ${String(error)}${error instanceof Error ? `\n${error.stack}` : ""}`);
    await exit(1);
  }
}
