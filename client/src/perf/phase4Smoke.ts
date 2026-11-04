import { invoke } from "@tauri-apps/api/core";
import * as bridge from "../bridge";
import { orderedRows } from "../store/catalog";
import { activity, subagentActivity } from "../store/session";
import { notices, perform } from "../store/actions";
import { composer } from "../store/composer";
import { terminal } from "../store/terminal";
import { processes } from "../store/processes";
import { questions } from "../store/questions";
import { detailTab, selectedKey, setDetailTab, setSelectedKey, subagentOpen } from "../store/ui";
import { flag, setSetting } from "../store/settings";
import type { Session } from "../ui/sessions/types";
import type { ActivityEvent } from "../types";
import { activityReadingRows } from "../ui/sessions/activity-anchor";
import { activityGeometry, activityGeometryValid, activitySelectionText, activityVirtualOffset, type ActivityGeometryReport } from "../ui/sessions/activity-virtual";


const API = "claude/sample-project/api-refactor";
const PAGINATION = "codex/sample-project/pagination";
const REVIEW = "claude/sample-project/review";
const DOCS = "claude/sample-project/docs-pass";
const FLAKY = "kimi/sample-project/flaky-test";
const RELEASE = "codex/sample-project/release-notes";
const QUESTION_HASH = "0123456789abcdef".repeat(4);
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";


interface SmokeEntry { argv: string[]; stdin: string }
interface AnswerRequest {
  question_id: string;
  expected_question_hash: string;
  expected_run_id: string;
  expected_conversation_id: string;
  answers: { question_id: string; selected_option_ids: string[]; text: string }[];
}
interface QueueRequest { queue_id: string }

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function until(ready: () => boolean, ms: number, what: string): Promise<void> {
  const started = performance.now();
  return new Promise((resolve, reject) => {
    const check = () => {
      if (ready()) resolve();
      else if (performance.now() - started > ms) reject(new Error(what));
      else setTimeout(check, 16);
    };
    check();
  });
}

async function untilAsync(ready: () => Promise<boolean>, ms: number, what: string): Promise<void> {
  const started = performance.now();
  while (true) {
    if (await ready()) return;
    if (performance.now() - started > ms) throw new Error(what);
    await sleep(40);
  }
}

function log(line: string) {
  void invoke("perf_log", { line }).catch(() => {});
}

async function calls(command: string): Promise<SmokeEntry[]> {
  const entries = await invoke<SmokeEntry[]>("smoke_log");
  return entries.filter((entry) => {
    const args = entry.argv[0]?.startsWith("@") ? entry.argv.slice(1) : entry.argv;
    return args[0] === command;
  });
}

async function waitCall(command: string, count: number, ms = 10_000): Promise<SmokeEntry[]> {
  await untilAsync(async () => (await calls(command)).length >= count, ms, `${command} was not called ${count} times`);
  return calls(command);
}

async function select(name: string): Promise<Session> {
  await until(() => orderedRows().some((row) => row.name === name), 30_000, `${name} did not appear`);
  const row = orderedRows().find((candidate) => candidate.name === name)!;
  setDetailTab("activity");
  setSelectedKey(row.key);
  await until(() => activity.target()?.id === row.key && activity.loaded() && composer.view()?.key === `${row.computer}\n${row.name}` && composer.view()?.loaded === true
    && composer.view()?.unavailable_reason !== "Checking the session…" && composer.view()?.block_reason !== "Checking the session…",
    15_000, `${name} did not open`)
    .catch((error) => {
      const view = composer.view();
      throw new Error(`${error.message}; ${JSON.stringify({ selected: selectedKey(), activityTarget: activity.target()?.id,
        activityLoaded: activity.loaded(), key: view?.key, loaded: view?.loaded, reason: view?.unavailable_reason, block: view?.block_reason })}`);
    });
  return row;
}

function editor(): HTMLTextAreaElement {
  const field = document.querySelector<HTMLTextAreaElement>(".cmp-editor");
  if (!field) throw new Error("the composer editor is missing");
  return field;
}

function type(text: string) {
  const field = editor();
  field.value = text;
  field.dispatchEvent(new InputEvent("input", { bubbles: true }));
}

function button(text: string, root: ParentNode = document): HTMLButtonElement {
  const found = [...root.querySelectorAll<HTMLButtonElement>("button")].find((candidate) => candidate.textContent?.trim() === text);
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

function menuAction(text: string): HTMLButtonElement {
  const found = [...document.querySelectorAll<HTMLButtonElement>(".menu button")].find((candidate) => candidate.textContent?.trim().startsWith(text));
  if (!found) throw new Error(`menu action not found: ${text}`);
  return found;
}

async function terminalStep() {
  await select(API);
  setDetailTab("terminal");
  await until(() => terminal.connected(), 15_000, "the Terminal did not connect");
  terminal.input(new TextEncoder().encode("echo smoke\r"));
  await until(() => document.querySelector(".term-host")?.textContent?.includes("echo smoke") === true, 10_000, "the Terminal did not show the echo");
  setDetailTab("activity");
}

async function savedDraftsDialogStep(goToSession = false) {
  const opener = document.querySelector<HTMLButtonElement>('button[aria-label="Saved drafts"]');
  if (!opener) throw new Error("the Saved drafts button is missing");
  opener.click();
  const dialog = () => document.querySelector<HTMLDialogElement>("dialog.saved-drafts");
  await until(() => dialog()?.open === true, 10_000, "Saved drafts did not open");
  if (!goToSession) {
    await until(() => document.activeElement === dialog()?.querySelector("[data-initial]"), 1000, "Saved drafts did not focus Close");
    button("Close", dialog()!).click();
    await until(() => dialog()?.open === false && document.activeElement === opener, 1000, "Saved drafts did not return focus");
    return;
  }
  await until(() => button("Go to session", dialog()!).getAttribute("aria-disabled") !== "true", 5000, "Go to session did not become available");
  button("Go to session", dialog()!).click();
  await until(() => dialog()?.open === false && document.activeElement === document.querySelector(".cmp-editor"), 5000, "Go to session did not focus the editor");
  const reopened = document.querySelector<HTMLButtonElement>('button[aria-label="Saved drafts"]');
  if (!reopened) throw new Error("the Saved drafts button is missing after Go to session");
  reopened.click();
  await until(() => dialog()?.open === true, 10_000, "Saved drafts did not reopen");
  button("Close", dialog()!).click();
  await until(() => dialog()?.open === false && document.activeElement === reopened, 1000, "Saved drafts did not return focus")
    .catch((error) => {
      const active = document.activeElement;
      throw new Error(`${error.message}; ${JSON.stringify({ active: active?.outerHTML.slice(0, 500), openerConnected: reopened.isConnected, dialogOpen: dialog()?.open, composerKey: composer.view()?.key, composerLoaded: composer.view()?.loaded })}`);
    });
}

async function draftsStep() {
  await select(API);
  type("phase 4 smoke");
  await until(() => composer.text() === "phase 4 smoke", 1000, "the editor did not take the text");
  await sleep(350);
  await select(PAGINATION);
  await select(API);
  if (composer.text() !== "phase 4 smoke") throw new Error("the draft did not survive navigation");
}

async function attachStep(path: string) {
  await select(API);
  const view = composer.view()!;
  const at = composer.selection().end;
  await bridge.composerAttach(view.handle, [path], at, at);
  await until(() => composer.view()?.attachments.some((file) => file.reference === "[File #1]") === true, 10_000, "the file did not attach");
}

async function sendStep(path: string) {
  await select(API);
  if (!composer.sendEnabled()) {
    const view = composer.view();
    throw new Error(`Send is disabled: ${JSON.stringify({ status: composer.status(), locked: composer.locked(), operationLocked: composer.operationLocked(),
      available: view?.available, reason: view?.unavailable_reason, block: view?.block_reason, attachments: view?.attachments.length,
      attachmentsAvailable: view?.attachments_available, attachmentReason: view?.attachments_reason, text: composer.text(),
      catalogTracked: orderedRows().find((row) => row.name === API)?.tracked })}`);
  }
  await composer.send();
  await until(() => composer.view()?.local.some((row) => row.status === "sent") === true, 10_000, "the local submitted row did not appear")
    .catch((error) => {
      const view = composer.view();
      throw new Error(`${error.message}; ${JSON.stringify({ status: view?.status, canSend: view?.can_send, sending: view?.sending,
        waiting: view?.waiting, uncertain: view?.uncertain, local: view?.local.map((row) => row.status), notices: notices().map((notice) => notice.text) })}`);
    });
  const submitted = composer.view()?.local.find((row) => row.status === "sent");
  if (!submitted || submitted.uncertain || submitted.error) throw new Error("the submitted row is not settled");
  const sent = await waitCall("send", 1);
  const name = path.split("/").pop()!;
  if (!sent[0].stdin.includes(name)) throw new Error("the attachment name is not in the send payload");
  if (sent[0].argv.some((word) => word.includes("phase 4 smoke"))) throw new Error("message text appeared in argv");
}

async function sendFollowStep() {
  await select(API);
  const scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  await until(() => scroller.scrollHeight > scroller.clientHeight + 200, 10_000, "Activity is too short for the follow check");
  scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -300, bubbles: true, cancelable: true }));
  scroller.scrollTo({ top: 0, behavior: "instant" });
  await sleep(100);
  type("follow smoke\nsecond line\nthird line");
  await composer.send();
  await until(() => composer.view()?.local.some((row) => row.status === "sent" && row.text === "follow smoke\nsecond line\nthird line") === true,
    10_000, "the follow-check send did not settle");
  const atEnd = () => Math.abs(scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight) < 2;
  await until(atEnd, 3000, "the sent row did not return to the end");
  const disclosure = scroller.querySelector<HTMLButtonElement>(".act-tools-head");
  if (!disclosure) throw new Error("the follow check needs a tool disclosure");
  const disclosureId = disclosure.closest<HTMLElement>(".act-tools")!.id;
  const disclosureStart = parseFloat(disclosure.closest<HTMLElement>(".act-virtual-row")!.dataset.start ?? "0");
  disclosure.click();
  await until(atEnd, 3000, "content growth did not stay at the end after send");
  const range = document.createRange();
  range.selectNodeContents(scroller.querySelector(".act-msg")!);
  const selection = document.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
  await sleep(100);
  const selectedTop = scroller.scrollTop, selectedHeight = scroller.scrollHeight;
  const selectedText = selection.toString();
  const originalScrollTo = scroller.scrollTo, scrollTo = originalScrollTo.bind(scroller);
  const writes: unknown[] = [];
  const frames: { top: number; height: number }[] = [];
  scroller.scrollTo = (options?: ScrollToOptions | number, y?: number) => {
    writes.push({ options, y });
    if (typeof options === "number") scrollTo(options, y ?? 0);
    else scrollTo(options);
  };
  try {
    disclosure.click();
    // Hold longer than the gesture idle timeout. New journal events must not
    // alter layout or invalidate the selected range until it is cleared.
    const target = activity.target()!;
    const seq = Math.max(0, ...activity.events().map((event) => event.seq ?? 0)) + 1;
    for (let n = 0; n < 30; n++) {
      if (n % 5 === 0) activity.applyActivity([{ computer: target.computer, key: target.key, kind: "events", payload: {
        events: [{ seq: seq + n, type: "Stop", source: "claude_hooks", detail: `Selection hold smoke ${n}`, at: 1_793_000_000 + seq + n }],
      } }]);
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      frames.push({ top: scroller.scrollTop, height: scroller.scrollHeight });
      log(`phase4-selection-frame ${JSON.stringify({ n, top: scroller.scrollTop, height: scroller.scrollHeight })}`);
    }
    await sleep(300);
    log(`phase4-selection-hold ${JSON.stringify({ selectedTop, selectedHeight, writes, frames: frames.length })}`);
    if (writes.length || frames.some((frame) => frame.top !== selectedTop || frame.height !== selectedHeight)
      || scroller.scrollTop !== selectedTop) throw new Error("a text selection did not pause Activity follow");
    if (selection.toString() !== selectedText) throw new Error("streaming invalidated the Activity selection");
  } finally {
    scroller.scrollTo = originalScrollTo;
    selection.removeAllRanges();
  }
  await until(() => atEnd() && (!disclosure.isConnected || disclosure.getAttribute("aria-expanded") === "false")
    && scroller.textContent?.includes("Selection hold smoke 25") === true, 3000, "clearing the selection did not resume Activity follow")
    .catch((error) => {
      throw new Error(`${error.message}: ${JSON.stringify({ atEnd: atEnd(), expanded: disclosure.getAttribute("aria-expanded"), disclosureConnected: disclosure.isConnected,
        appended: scroller.textContent?.includes("Selection hold smoke 25"), top: scroller.scrollTop, height: scroller.scrollHeight, viewport: scroller.clientHeight,
        placed: scroller.querySelector<HTMLDivElement>(".act-col")?.style.visibility, rows: scroller.querySelectorAll(".act-virtual-row").length })}`);
    });
  if (!disclosure.isConnected) {
    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -300, bubbles: true, cancelable: true }));
    scroller.scrollTo({ top: disclosureStart, behavior: "instant" });
    await until(() => document.getElementById(disclosureId)?.querySelector(".act-tools-head")?.getAttribute("aria-expanded") === "false",
      3000, "the collapsed disclosure did not retain its state after virtual remounting");
  }
  const again = document.createRange();
  again.selectNodeContents(scroller.querySelector(".act-msg")!);
  selection.addRange(again);
  await sleep(100);
  await select(PAGINATION);
  await until(() => {
    const next = document.querySelector<HTMLDivElement>(".act-scroll");
    return next?.querySelector<HTMLDivElement>(".act-col")?.style.visibility === "visible" && selection.isCollapsed
      && Math.abs(next.scrollHeight - next.scrollTop - next.clientHeight) < 2;
  }, 3000, "the old selection blocked the next session's first placement");
}

async function activityAnchorStep() {
  await setSetting("workspace/reduceMotion", true, { persist: false });
  await select(DOCS);
  const scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  await until(() => scroller.querySelectorAll(".act-virtual-row").length > 0
    && scroller.querySelector<HTMLElement>(".act-col")?.style.visibility === "visible", 10_000, "the measured Activity did not reveal");
  const before = activity.events()[0]?.seq ?? 0;
  await until(() => scroller.scrollTop > 0, 5000, "Activity did not reach its initial end position");
  const animated = scroller.querySelector<HTMLElement>(".act-virtual-row")!;
  await until(() => animated.getAnimations({ subtree: true }).length === 0, 3000, "the virtual row animation did not settle");
  const row = scroller.querySelector<HTMLElement>(".act-virtual-row")!;
  const start = parseFloat(row.dataset.start ?? "0");
  const key = row.querySelector<HTMLElement>("[id^=item-]")!.id;
  const formula = activityVirtualOffset(scroller, row.closest<HTMLElement>(".act-virtual")!, start), dom = activityReadingRows(scroller).offset(key);
  if (scroller.scrollTop < 1000 || dom === undefined || Math.abs(formula - dom) >= 1) {
    throw new Error(`the virtual measured formula disagrees with the DOM: ${JSON.stringify({ formula, dom, scrollTop: scroller.scrollTop })}`);
  }
  scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -300, bubbles: true, cancelable: true }));
  scroller.scrollTo({ top: 0, behavior: "instant" });
  await new Promise(requestAnimationFrame);
  const marker = activityReadingRows(scroller).visible()[0];
  if (!marker) throw new Error("the anchor check has no visible item");
  const id = marker.key, top = marker.offset;
  await until(() => (activity.events()[0]?.seq ?? before) < before, 10_000, "the older Activity page did not arrive");
  await until(() => {
    const offset = activityReadingRows(scroller).offset(id);
    return offset !== undefined && Math.abs(offset - top) < 3;
  }, 3000, "the older-page anchor did not settle")
    .catch((error) => {
      throw new Error(`${error.message}: ${JSON.stringify({ id, top, captured: marker.offset, currentTop: activityReadingRows(scroller).offset(id),
        scrollTop: scroller.scrollTop, height: scroller.scrollHeight, rows: scroller.querySelectorAll(".act-virtual-row").length })}`);
    });
}

async function readingAppendStep() {
  const scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  document.querySelector<HTMLButtonElement>('button[aria-label^="Jump to latest activity"]')?.click();
  await until(() => scroller.scrollTop > 1000, 5000, "Activity did not return to a deep reading position");
  const before = scroller.scrollTop;
  scroller.focus();
  scroller.dispatchEvent(new KeyboardEvent("keydown", { key: "PageUp", bubbles: true, cancelable: true }));
  // Untrusted keys do not scroll WebKit, so apply the Page Up viewport movement explicitly.
  scroller.scrollBy({ top: -scroller.clientHeight, behavior: "instant" });
  await until(() => scroller.scrollTop < before, 3000, "Page Up did not move Activity")
    .catch((error) => {
      throw new Error(`${error.message}: ${JSON.stringify({ before, after: scroller.scrollTop, active: document.activeElement?.className })}`);
    });
  const edge = () => scroller.getBoundingClientRect().top + scroller.clientTop;
  await until(() => [...scroller.querySelectorAll<HTMLElement>(".act-virtual-row [id^=item-]")].some((item) => {
    const rect = item.getBoundingClientRect();
    return rect.bottom > edge() && rect.top < edge() + scroller.clientHeight;
  }), 3000, "Page Up did not render a reading row");
  const marker = [...scroller.querySelectorAll<HTMLElement>(".act-virtual-row [id^=item-]")].find((item) => {
    const rect = item.getBoundingClientRect();
    return rect.bottom > edge() && rect.top < edge() + scroller.clientHeight;
  });
  if (!marker) throw new Error("the append check has no visible reading row");
  const id = marker.id, top = marker.getBoundingClientRect().top - edge();
  const seq = Math.max(...activity.events().map((event) => event.seq ?? 0)) + 1;
  const at = Math.max(...activity.events().map((event) => event.at ?? 0)) + 1;
  activity.applyActivity([{ computer: "", key: "run:000003-run", kind: "events", payload: {
    events: [{ seq, type: "Stop", source: "claude_transcript", detail: "append while reading smoke", at }], cursor: seq,
  } }]);
  await until(() => activity.events().some((event) => event.seq === seq), 1000, "the appended event did not arrive");
  await until(() => {
    const current = document.getElementById(id);
    return !!current && Math.abs(current.getBoundingClientRect().top - edge() - top) < 1;
  }, 3000, "an append moved the released reading anchor")
    .catch((error) => {
      const current = document.getElementById(id);
      throw new Error(`${error.message}: ${JSON.stringify({ id, top, currentTop: current ? current.getBoundingClientRect().top - edge() : undefined,
        scrollTop: scroller.scrollTop, height: scroller.scrollHeight, rows: scroller.querySelectorAll(".act-virtual-row").length })}`);
    });
}

async function activeScrollStep() {
  await select(DOCS);
  const scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  await until(() => scroller.scrollTop > 1000, 5000, "the active scroll check did not start deep in Activity");
  await sleep(300);
  const original = scroller.scrollTo;
  const writes: unknown[] = [];
  let maxDrift = 0, frames = 0;
  scroller.scrollTo = function (this: HTMLElement, ...args: Parameters<HTMLElement["scrollTo"]>) {
    writes.push(args);
    return original.apply(this, args);
  } as HTMLElement["scrollTo"];
  try {
    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -300, bubbles: true, cancelable: true }));
    for (let index = 0; index < 48; index++) {
      const top = scroller.scrollTop;
      const before = activityReadingRows(scroller).visible();
      const requested = Math.max(0, top - 16);
      (original as (options: ScrollToOptions) => void).call(scroller, { top: requested, behavior: "instant" });
      if (index % 8 === 0) {
        const target = activity.target()!;
        const seq = Math.max(0, ...activity.events().map((event) => event.seq ?? 0)) + 1;
        activity.applyActivity([{ computer: target.computer, key: target.key, kind: "events", payload: {
          events: [{ seq, type: "AgentMessage", source: "claude_transcript", detail: `Active scroll append ${index}`, at: 1793000200 + index }], cursor: seq,
        } }]);
      }
      await new Promise<void>((resolve) => requestAnimationFrame(() => setTimeout(resolve, 0)));
      const anchor = before.find((row) => activityReadingRows(scroller).offset(row.key) !== undefined);
      if (!anchor) throw new Error("active scrolling lost every visible reading row");
      const offset = activityReadingRows(scroller).offset(anchor.key)!;
      // Compare pixels to the user's requested movement. Native anchoring may
      // compensate scrollTop for a measured row above; app writes remain forbidden.
      const drift = Math.abs(offset - (anchor.offset - (requested - top)));
      maxDrift = Math.max(maxDrift, drift); frames++;
      if (drift > 1) throw new Error(`an active virtual measurement shifted the reading row: ${JSON.stringify({ key: anchor.key, drift, index })}`);
    }
    if (writes.length) throw new Error(`Activity wrote scroll position during an active gesture: ${JSON.stringify(writes)}`);
    log(`phase4-active-scroll ${JSON.stringify({ frames, maxDrift, writes: writes.length })}`);
  } finally { scroller.scrollTo = original; }
  await sleep(350);
}

/** Long Markdown, deferred measurements and appends during downward wheel
 * gestures. A released reader must be able to advance through every row. */
async function variableScrollStep() {
  await select(DOCS);
  const scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  const target = activity.target()!;
  const savedEvents = activity.events(), savedSummary = activity.summary();
  let seq = 1;
  const markdown = (index: number) => `## Response ${index}\n\n` + Array.from({ length: 3 + index % 9 }, (_, block) =>
    `Paragraph ${block}: a variable-height Markdown reply with **emphasis**, a [fixture link](https://example.org/) and enough words to wrap across the reading column.\n\n`
    + `1. First list entry with another line that wraps in the reading column.\n2. Second entry\n   - Nested entry ${index}\n\n`
    + "```rust\nfn fixture() {\n    let value = 42;\n    println!(\"{value}\");\n}\n```\n\n").join("");
  const staticWheel = new URLSearchParams(location.search).get("phase4-static-wheel") === "1";
  const rolling = new URLSearchParams(location.search).get("phase4-provider-window") === "1";
  const shape = new URLSearchParams(location.search).get("phase4-static-shape") ?? "messages";
  let events = Array.from({ length: rolling ? 100 : 56 }, (_, index) => ({ seq: seq++, type: "AgentMessage", source: "claude_transcript", message_id: `variable-${index}`, at: 1793000000 + index, detail: markdown(index) }));
  if (staticWheel && shape !== "messages") {
    events = []; seq = 1;
    for (let index = 0; index < 72; index++) {
      events.push({ seq: seq++, type: index % 2 ? "UserMessage" : "AgentMessage", source: "claude_transcript", message_id: `variable-${index}`, at: 1793000000 + index, detail: index % 2 ? `Short request ${index}` : markdown(index).repeat(1 + index % 3) + "\n\n| Column | Value |\n| --- | --- |\n| Layout | Variable height |\n" });
      for (let tool = 0; tool < 6; tool++) {
        events.push({ seq: seq++, type: "PreToolUse", source: "claude_hooks", message_id: `tool-${index}-${tool}`, at: 1793000000 + index, detail: `printf 'fixture ${index} ${tool}'\n` });
        events.push({ seq: seq++, type: "PostToolUse", source: "claude_hooks", message_id: `result-${index}-${tool}`, at: 1793000000 + index, detail: `fixture output ${index} ${tool}\n` });
      }
    }
  }
  let provider = rolling ? events.map(event => ({ ...event, seq: undefined })) : [];
  let providerIndex = provider.length;
  const publishProvider = () => activity.applyActivity([{ computer: target.computer, key: target.key, kind: "state", payload: { rev: 1_000_000 + providerIndex,
    value: { provider_messages: provider, message_events: [], conversation_id: target.conversationId } } }]);
  activity.applyActivity([{ computer: target.computer, key: target.key, kind: "replace", payload: { events: rolling ? [] : events } }]);
  if (rolling) publishProvider();
  else activity.applyActivity([{ computer: target.computer, key: target.key, kind: "state", payload: { rev: Number.MAX_SAFE_INTEGER, value: { provider_messages: [], message_events: [], conversation_id: target.conversationId } } }]);
  if (staticWheel && shape === "expanded") {
    await sleep(0);
    const buttons = [...scroller.querySelectorAll<HTMLButtonElement>(".act-tools-head")];
    for (const button of buttons) button.click();
    log(`phase4-expanded-fixture ${JSON.stringify({ buttons: buttons.length })}`);
  }
  await until(() => document.querySelector(".act-col")?.getAttribute("style")?.includes("visible") === true && scroller.scrollHeight > 20000, 10000, "variable Markdown did not settle").catch(error => { throw new Error(`${error.message}: ${JSON.stringify({ hidden: document.hidden, focused: document.hasFocus(), innerWidth, innerHeight, height: scroller.scrollHeight, viewport: scroller.clientHeight, top: scroller.scrollTop, column: document.querySelector(".act-col")?.getAttribute("style"), mounted: scroller.querySelectorAll(".act-virtual-row").length })}`); });
  await sleep(500);
  const original = scroller.scrollTo;
  const originalBy = scroller.scrollBy;
  const originalIntoView = Element.prototype.scrollIntoView;
  const originalFocus = HTMLElement.prototype.focus;
  const topProperty = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
  const writes: { method: string; top: number; before: number; max: number; at: number; active: boolean; stack?: string }[] = [];
  const trace = (method: string, top: number) => {
    const entry = { method, top, before: scroller.scrollTop, max: scroller.scrollHeight - scroller.clientHeight, at: performance.now(), active: performance.now() - lastWheel < 250, stack: new Error().stack };
    writes.push(entry);
    if (staticWheel) { log(`phase4-scroll-write ${JSON.stringify({ ...entry, stack: undefined })}`); log(`phase4-scroll-stack ${entry.stack?.slice(0, 450)}`); }
  };
  Object.defineProperty(scroller, "scrollTop", { configurable: true, get() { return topProperty.get!.call(scroller); }, set(top: number) { trace("scrollTop", top); topProperty.set!.call(scroller, top); } });
  scroller.scrollBy = function (...args: Parameters<HTMLElement["scrollBy"]>) { trace("scrollBy", scroller.scrollTop); return originalBy.apply(scroller, args); } as HTMLElement["scrollBy"];
  Element.prototype.scrollIntoView = function (...args) { if (this === scroller || scroller.contains(this)) trace("scrollIntoView", scroller.scrollTop); return originalIntoView.apply(this, args); };
  HTMLElement.prototype.focus = function (...args) { if (this === scroller || scroller.contains(this)) trace("focus", scroller.scrollTop); return originalFocus.apply(this, args); };
  let lastWheel = -Infinity, backtracks = 0, samples = 0;
  scroller.scrollTo = function (this: HTMLElement, ...args: Parameters<HTMLElement["scrollTo"]>) {
    const values = args as unknown as [ScrollToOptions | number, number?];
    trace("scrollTo", typeof values[0] === "number" ? values[1] ?? 0 : values[0]?.top ?? 0);
    return original.apply(this, args);
  } as HTMLElement["scrollTo"];
  try {
    lastWheel = performance.now();
    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -10000, bubbles: true, cancelable: true }));
    (original as (options: ScrollToOptions) => void).call(scroller, { top: rolling ? (scroller.scrollHeight - scroller.clientHeight) * .4 : 8000, behavior: "instant" });
    await sleep(400);
    writes.length = 0;
    // Model compositor read-back arriving before its queued DOM scroll event.
    // The original native method is the input driver; only application writes
    // are traced. A concurrent provider update must keep that accepted position.
    if (!staticWheel) {
    const accepted = scroller.scrollTop + 140;
    (original as (options: ScrollToOptions) => void).call(scroller, { top: accepted, behavior: "instant" });
    if (rolling) {
      provider = [...provider]; providerIndex++;
      publishProvider();
    } else activity.applyActivity([{ computer: target.computer, key: target.key, kind: "events", payload: {
      events: [{ seq: seq++, type: "AgentMessage", source: "claude_transcript", detail: markdown(0), at: 1793000999 }], cursor: seq - 1,
    } }]);
    const immediately = scroller.scrollTop;
    log(`phase4-native-scroll-before-layout ${JSON.stringify({ accepted, immediately, writes: writes.slice(0, 4) })}`);
    if (Math.abs(immediately - accepted) > 1) throw new Error(`a layout callback reversed browser scrolling before its scroll event: ${accepted} -> ${immediately}`);
    writes.length = 0;
    }
    if (new URLSearchParams(location.search).get("phase4-native-wheel") === "1") {
      let trusted = 0;
      const deltas: unknown[] = [];
      const wheel = (event: WheelEvent) => { lastWheel = performance.now(); if (event.isTrusted) trusted++; if (deltas.length < 8) deltas.push({ y: event.deltaY, mode: event.deltaMode }); };
      scroller.addEventListener("wheel", wheel, { capture: true });
      const rect = scroller.getBoundingClientRect();
      log(`phase4-native-wheel-ready ${JSON.stringify({ staticWheel, x: Math.round(rect.left + rect.width * .7), y: Math.round(rect.top + rect.height * .5), top: scroller.scrollTop, max: scroller.scrollHeight - scroller.clientHeight, first: activityReadingRows(scroller).visible()[0] })}`);
      let previous = activityReadingRows(scroller).visible()[0];
      const staticEvents = activity.events();
      const initialHeight = scroller.scrollHeight;
      let minHeight = initialHeight, maxHeight = initialHeight;
      const positions: unknown[] = [];
      try {
        const deadline = performance.now() + 22000;
        for (let index = 0; staticWheel ? performance.now() < deadline : index < 240; index++) {
          if (!staticWheel && !rolling && index % 19 === 0) {
            const changed = activity.events().map((event, row) => row < 2 ? { ...event, detail: markdown(row).repeat(1 + index % 3) } : event);
            activity.applyActivity([{ computer: target.computer, key: target.key, kind: "replace", payload: { events: changed } }]);
          }
          if (!staticWheel && index % 11 === 0) {
            if (rolling) {
              provider = [...provider.slice(1), { seq: undefined, type: "AgentMessage", source: "claude_transcript", message_id: `variable-${providerIndex++}`, at: 1793001000 + index, detail: markdown(index) }];
              publishProvider();
            } else activity.applyActivity([{ computer: target.computer, key: target.key, kind: "events", payload: {
              events: [{ seq: seq++, type: "AgentMessage", source: "claude_transcript", message_id: `native-append-${index}`, at: 1793001000 + index, detail: markdown(index) }], cursor: seq - 1,
            } }]);
          }
          if (staticWheel) await new Promise<void>(resolve => requestAnimationFrame(() => setTimeout(resolve, 0)));
          else await sleep(60);
          if (staticWheel && activity.events() !== staticEvents) throw new Error("static fixture received an unexpected event-window update");
          minHeight = Math.min(minHeight, scroller.scrollHeight); maxHeight = Math.max(maxHeight, scroller.scrollHeight);
          const current = activityReadingRows(scroller).visible()[0];
          positions.push({ index, at: performance.now(), first: current, top: scroller.scrollTop, max: scroller.scrollHeight - scroller.clientHeight });
          if (positions.length > 24) positions.shift();
          const rank = (key: string) => Number(key.match(/(?:variable-|item-s)([0-9]+)$/)?.[1] ?? NaN);
          if (previous && current && ((current.key === previous.key && current.offset > previous.offset + 1) || rank(current.key) < rank(previous.key))) {
            backtracks++; log(`phase4-native-backtrack ${JSON.stringify({ index, previous, current, top: scroller.scrollTop, max: scroller.scrollHeight - scroller.clientHeight })}`);
          }
          previous = current; samples++;
        }
      } finally { scroller.removeEventListener("wheel", wheel, { capture: true }); }
      const activeWrites = writes.filter(write => write.active);
      const heightDrift = Math.max(maxHeight - initialHeight, initialHeight - minHeight);
      log(`phase4-static-geometry ${JSON.stringify({ shape, initialHeight, minHeight, maxHeight, heightDrift })}`);
      log(`phase4-native-wheel ${JSON.stringify({ staticWheel, shape, rolling, trusted, deltas, samples, backtracks, activeWrites: activeWrites.length, writes: writes.slice(0, 12), top: scroller.scrollTop, max: scroller.scrollHeight - scroller.clientHeight, events: activity.events().length, firstSeq: activity.events()[0]?.seq, lastSeq: activity.events().at(-1)?.seq })}`);
      if (staticWheel && heightDrift > 1) throw new Error(`static transcript changed its scroll range as rows remounted: ${JSON.stringify({ initialHeight, minHeight, maxHeight, heightDrift })}`);
      if (!trusted || backtracks || activeWrites.length) throw new Error(`native wheel-down snapback: ${JSON.stringify({ trusted, samples, backtracks, activeWrites: activeWrites.length, writes: writes.slice(0, 4) })}`);
      return;
    }
    for (let index = 0; index < 90; index++) {
      lastWheel = performance.now();
      scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: 140, bubbles: true, cancelable: true }));
      const requested = scroller.scrollTop + 140;
      (original as (options: ScrollToOptions) => void).call(scroller, { top: requested, behavior: "instant" });
      if (index % 9 === 0) activity.applyActivity([{ computer: target.computer, key: target.key, kind: "events", payload: {
        events: [{ seq: seq++, type: "AgentMessage", source: "claude_transcript", message_id: `append-${index}`, at: 1793001000 + index, detail: markdown(index) }], cursor: seq - 1,
      } }]);
      await new Promise<void>(resolve => requestAnimationFrame(() => setTimeout(resolve, 0)));
      if (scroller.scrollTop < requested - 1) backtracks++;
      samples++;
      if (index % 7 === 0) await sleep(400);
    }
    await sleep(500);
    const activeWrites = writes.filter(write => write.active);
    log(`phase4-variable-scroll ${JSON.stringify({ samples, backtracks, activeWrites: activeWrites.length, writes: writes.slice(0, 12), top: scroller.scrollTop })}`);
    if (backtracks || activeWrites.length) throw new Error(`wheel-down snapback: ${JSON.stringify({ samples, backtracks, activeWrites: activeWrites.length, writes: writes.slice(0, 4) })}`);
  } finally {
    scroller.scrollTo = original; scroller.scrollBy = originalBy;
    delete (scroller as unknown as { scrollTop?: number }).scrollTop;
    Element.prototype.scrollIntoView = originalIntoView; HTMLElement.prototype.focus = originalFocus;
    activity.applyActivity([{ computer: target.computer, key: target.key, kind: "replace", payload: { events: savedEvents } },
      { computer: target.computer, key: target.key, kind: "state", payload: { rev: Number.MAX_SAFE_INTEGER, value: savedSummary } }]);
    await sleep(400);
  }
}

async function longSelectionCopyStep() {
  await select(DOCS);
  const scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -300, bubbles: true, cancelable: true }));
  scroller.scrollTo({ top: 0, behavior: "instant" });
  await sleep(50);
  const visible = () => [...scroller.querySelectorAll<HTMLElement>(".act-virtual-row")].filter((row) => {
    const rect = row.getBoundingClientRect(), edge = scroller.getBoundingClientRect().top;
    return rect.bottom > edge && rect.top < edge + scroller.clientHeight;
  });
  const first = visible()[0], start = Number(first?.dataset.index);
  if (!first || !Number.isSafeInteger(start)) throw new Error("the long selection has no initial row");
  const selection = document.getSelection()!;
  first.dispatchEvent(new PointerEvent("pointerdown", { button: 0, bubbles: true, pointerType: "mouse" }));
  selection.setBaseAndExtent(first, 0, first, first.childNodes.length);
  const noticesBefore = new Set(notices().map((notice) => notice.id));
  try {
    await sleep(30);
    let last = start;
    for (let step = 0; step < 10; step++) {
      scroller.scrollBy({ top: scroller.clientHeight * .8, behavior: "instant" });
      await new Promise<void>((resolve) => requestAnimationFrame(() => setTimeout(resolve, 0)));
      const end = visible().at(-1);
      if (!end) throw new Error("the long drag lost its visible endpoint");
      selection.setBaseAndExtent(first, 0, end, end.childNodes.length);
      await sleep(30); // Deliver selectionchange before moving the virtual window again.
      last = Number(end.dataset.index);
      for (let index = start; index <= last; index++) {
        if (!scroller.querySelector(`.act-virtual-row[data-index="${index}"]`)) {
          throw new Error(`the long selection unmounted middle row ${index} of ${start}..${last}`);
        }
      }
      if (last - start >= 28) break; // Well beyond both overscan windows.
    }
    if (last - start < 24) throw new Error(`the drag did not cross enough rows: ${start}..${last}`);
    const expected = selection.toString();
    if (expected.length < 500 || activitySelectionText(scroller) !== expected) throw new Error("the long partial selection lost its text");
    if (!scroller.dispatchEvent(new KeyboardEvent("keydown", { key: "c", ctrlKey: true, bubbles: true, cancelable: true }))) {
      throw new Error("the long partial selection intercepted native Ctrl+C");
    }
    scroller.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 400, clientY: 300 }));
    await until(() => !!document.querySelector(".menu"), 1000, "the long-selection Copy menu did not open");
    // Menu focus may collapse WebKit's live range; Activity captures the full
    // selection when the context menu opens, before moving focus to its buttons.
    menuAction("Copy").click();
    // Rust's private-X11 native clipboard path reads back and verifies its write.
    await until(() => notices().some((notice) => !noticesBefore.has(notice.id) && notice.text === "The selected text is on the clipboard"),
      5000, "the long-selection clipboard write did not succeed");
    log(`phase4-long-selection-copy ${JSON.stringify({ start, last, rows: last - start + 1, characters: expected.length })}`);
  } finally {
    selection.removeAllRanges();
    window.dispatchEvent(new PointerEvent("pointerup", { button: 0, pointerType: "mouse" }));
  }
  await sleep(350);
}

async function selectAllCopyStep() {
  const scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  document.querySelector<HTMLButtonElement>('button[aria-label^="Jump to latest activity"]')?.click();
  const users = activity.events().filter((event) => event.type === "UserPromptSubmit" && event.detail);
  const first = users[0]?.detail, last = users.at(-1)?.detail;
  if (!first || !last) throw new Error("the copy check needs loaded user messages at both ends");
  // Repeated passing notices coalesce on the bus, so count copies by their notice ids.
  const existingNotices = new Set(notices().map((notice) => notice.id));
  const copyIds = new Set<number>();
  const copyNotices = () => {
    for (const notice of notices()) if (!existingNotices.has(notice.id) && notice.text === "The selected text is on the clipboard") copyIds.add(notice.id);
    return copyIds.size;
  };
  const writes: string[] = [];
  scroller.focus();
  scroller.dispatchEvent(new KeyboardEvent("keydown", { key: "a", ctrlKey: true, bubbles: true, cancelable: true }));
  const mounted = document.getSelection()?.toString() ?? "";
  scroller.dispatchEvent(new KeyboardEvent("keydown", { key: "c", ctrlKey: true, bubbles: true, cancelable: true }));
  await until(() => copyNotices() === 1, 3000, "Ctrl+C did not copy the transcript");
  writes.push(activitySelectionText(scroller));
  if (!writes[0].includes(first) || !writes[0].includes(last)) {
    throw new Error(`Select All copied only the mounted window: ${JSON.stringify({ copied: writes[0].length, mounted: mounted.length })}`);
  }
  scroller.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 400, clientY: 300 }));
  await until(() => !!document.querySelector(".menu"), 1000, "the Activity menu did not open");
  menuAction("Select All").click();
  const menuText = activitySelectionText(scroller);
  scroller.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 400, clientY: 300 }));
  await until(() => !!document.querySelector(".menu"), 1000, "the Activity menu did not reopen");
  menuAction("Copy").click();
  await until(() => copyNotices() === 2, 3000, "the Activity menu did not copy the transcript");
  writes.push(menuText);
  if (writes[1] !== writes[0]) throw new Error(`menu Copy and Ctrl+C returned different full transcripts: ${JSON.stringify({ keyboard: writes[0].length, menu: writes[1].length,
    keyboardFirst: writes[0].includes(first), keyboardLast: writes[0].includes(last), menuFirst: writes[1].includes(first), menuLast: writes[1].includes(last) })}`);

  const edge = scroller.getBoundingClientRect().top;
  const message = [...scroller.querySelectorAll<HTMLElement>(".act-msg")].find((item) => {
    const rect = item.getBoundingClientRect();
    return rect.bottom > edge && rect.top < edge + scroller.clientHeight && !!item.textContent?.trim();
  });
  if (!message) throw new Error("the partial-copy check has no visible message");
  const range = document.createRange();
  range.selectNodeContents(message);
  const selection = document.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
  const partial = selection.toString();
  const nativeCopy = scroller.dispatchEvent(new KeyboardEvent("keydown", { key: "c", ctrlKey: true, bubbles: true, cancelable: true }));
  if (!nativeCopy) throw new Error("a partial selection was intercepted instead of using native copy");
  await sleep(50);
  if (copyNotices() !== 2) throw new Error("a partial selection used the custom clipboard path");
  if (!partial || partial === writes[0]) throw new Error(`the partial selection did not replace Select All: ${JSON.stringify({ partial: partial.length, full: writes[0].length, connected: message.isConnected, visibility: getComputedStyle(message).visibility })}`);
  scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -200, bubbles: true, cancelable: true }));
  scroller.scrollBy({ top: -200, behavior: "instant" });
  await sleep(100);
  if (selection.toString() !== partial) throw new Error("scrolling unmounted a selected message");
  selection.removeAllRanges();
}

async function mediaPlaybackStep() {
  await select(API);
  const scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  await until(() => !!document.querySelector(".act-msg.media"), 5000, "the playback media row did not appear");
  const card = document.querySelector<HTMLElement>(".act-msg.media")!;
  card.querySelector<HTMLButtonElement>('button[aria-label="Show transcript"]')?.click();
  const play = card.querySelector<HTMLButtonElement>(".vp-btn")!;
  const position = () => Number(card.querySelector(".vp-wave")!.getAttribute("aria-valuenow"));
  play.click();
  await until(() => position() > 0 && play.getAttribute("aria-pressed") === "true", 2000, "voice playback did not start");
  const before = position();
  scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -300, bubbles: true, cancelable: true }));
  scroller.scrollTo({ top: 0, behavior: "instant" });
  await sleep(350);
  if (!card.isConnected || document.getElementById(card.id) !== card || play.getAttribute("aria-pressed") !== "true" || position() <= before) {
    throw new Error(`scrolling stopped or reset the playing row: ${JSON.stringify({ before, after: position(), connected: card.isConnected, pressed: play.getAttribute("aria-pressed") })}`);
  }
  if (card.querySelector(".vm-text")?.textContent !== "media smoke transcript") throw new Error("scrolling closed the media transcript");
  scroller.scrollTo({ top: scroller.scrollHeight, behavior: "instant" });
  await until(() => {
    const rect = card.getBoundingClientRect();
    return card.isConnected && document.getElementById(card.id) === card && rect.top >= 0 && rect.bottom <= innerHeight;
  }, 3000, "the playing row did not return to the viewport");
  const stop = card.querySelector<HTMLButtonElement>(".vp-btn")!;
  stop.click();
  await until(() => stop.getAttribute("aria-pressed") === "false", 1000, "playback did not stop after the check")
    .catch((error) => {
      throw new Error(`${error.message}: ${JSON.stringify({ pressed: stop.getAttribute("aria-pressed"), position: position(), connected: card.isConnected })}`);
    });
}

function watchActivityGeometry() {
  let active = true, frames = 0, populated = 0, badFrames = 0, phase = "initial";
  const violations: { phase: string; at: number; report: ActivityGeometryReport }[] = [];
  const compact = (report: ActivityGeometryReport) => {
    const overlap = report.overlaps[0];
    return { rows: report.rows.length, visible: report.visible.length, gap: report.firstVisibleGap,
      overlap: overlap && { above: overlap.above.index, below: overlap.below.index, pixels: overlap.pixels, virtualPixels: overlap.virtualPixels,
        aboveTop: overlap.above.top, aboveBottom: overlap.above.bottom, belowTop: overlap.below.top,
        starts: [overlap.above.start, overlap.below.start], domStarts: [overlap.above.domStart, overlap.below.domStart], heights: [overlap.above.height, overlap.below.height] } };
  };
  const sample = () => {
    if (!active) return;
    const scroller = document.querySelector<HTMLElement>(".act-scroll");
    if (scroller?.querySelector<HTMLElement>(".act-col")?.style.visibility === "visible") {
      const report = activityGeometry(scroller);
      frames++;
      if (report.rows.length && report.visible.length) populated++;
      if (report.rows.length && !activityGeometryValid(report)) {
        badFrames++;
        if (violations.length < 8) {
          violations.push({ phase, at: performance.now(), report });
          log(`phase4-geometry-violation ${JSON.stringify({ phase, ...compact(report) })}`);
        }
      }
    }
    requestAnimationFrame(() => { setTimeout(sample, 0); });
  };
  requestAnimationFrame(() => { setTimeout(sample, 0); });
  return {
    mark(name: string) { phase = name; log(`phase4-geometry-stage ${name}`); },
    stop() {
      active = false;
      log(`phase4-geometry-summary ${JSON.stringify({ frames, populated, badFrames })}`);
      if (!populated) throw new Error("the geometry campaign captured no visible virtual rows");
      if (badFrames) throw new Error(`Activity geometry failed on ${badFrames} frames: ${JSON.stringify({ phase: violations[0].phase, ...compact(violations[0].report) })}`);
    },
  };
}

async function activityGeometryStep() {
  await setSetting("workspace/reduceMotion", true, { persist: false });
  await select(DOCS);
  await until(() => document.querySelectorAll(".act-virtual-row").length > 0, 10_000, "the geometry Activity did not mount");
  const watch = watchActivityGeometry();
  const nextFrames = async (count: number) => {
    for (let index = 0; index < count; index++) await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  };
  let restoreSize: { width: number; height: number } | undefined;
  let loadedFont: FontFace | undefined;
  try {
    const target = activity.target()!;
    let seq = Math.max(0, ...activity.events().map((event) => event.seq ?? 0)) + 1;
    const at = Math.max(0, ...activity.events().map((event) => event.at ?? 0)) + 1;
    for (let round = 0; round < 6; round++) {
      watch.mark(`append-${round}`);
      const peer = `Message from claude/sample-project/peer ${round}\n\nA long peer reply wraps across the reading column. `.repeat(18);
      const agent = `Agent response ${round} follows the collapsed tool group with another multiline answer. `.repeat(16);
      const events = [
        { seq: seq++, type: "Stop", source: "hgs_wait", origin: "peer_message", sender: "claude/sample-project/peer", detail: peer, at: at + seq },
        { seq: seq++, type: "PreToolUse", source: "claude_hooks", tool: "Bash", detail: `printf 'geometry ${round}'\n`, at: at + seq },
        { seq: seq++, type: "PostToolUse", source: "claude_hooks", tool: "Bash", detail: `geometry ${round}\n`, at: at + seq },
        { seq: seq++, type: "AgentMessage", source: "claude_transcript", detail: agent, at: at + seq },
      ];
      activity.applyActivity([{ computer: target.computer, key: target.key, kind: "events", payload: { events, cursor: seq - 1 } }]);
      await nextFrames(3);
    }
    watch.mark("late-growth");
    const changed = activity.events().map((event) => ({ ...event }));
    const peer = [...changed].reverse().find((event) => event.origin === "peer_message");
    if (!peer) throw new Error("the geometry campaign did not create a peer message");
    peer.detail = `${peer.detail}\n\nLate wrapping growth after the row was measured. `.repeat(24);
    activity.applyActivity([{ computer: target.computer, key: target.key, kind: "replace", payload: { events: changed } }]);
    await nextFrames(6);

    watch.mark("window-resize");
    restoreSize = { width: innerWidth, height: innerHeight };
    await invoke("perf_resize", { width: Math.max(760, restoreSize.width - 240), height: restoreSize.height });
    await nextFrames(8);
    await invoke("perf_resize", restoreSize);
    restoreSize = undefined;
    await nextFrames(8);

    watch.mark("font-load");
    loadedFont = await new FontFace("Activity Geometry Smoke", 'local("Noto Sans Mono")').load();
    document.fonts.add(loadedFont);
    const column = document.querySelector<HTMLElement>(".act-scroll .act-col")!;
    column.style.fontFamily = '"Activity Geometry Smoke"';
    await nextFrames(8);
    column.style.fontFamily = "";
    document.fonts.delete(loadedFont);
    loadedFont = undefined;
    await nextFrames(6);

    watch.mark("cached-switch-away");
    await select(PAGINATION);
    await nextFrames(6);
    watch.mark("cached-switch-back");
    await select(DOCS);
    await nextFrames(10);
  } finally {
    const column = document.querySelector<HTMLElement>(".act-scroll .act-col");
    if (column) column.style.fontFamily = "";
    if (loadedFont) document.fonts.delete(loadedFont);
    try {
      if (restoreSize) await invoke("perf_resize", restoreSize);
    } finally {
      watch.stop();
    }
  }
}

/** Exercise real WebKit scroll clamping, not only settled row measurements. */
async function activityBottomStep() {
  await setSetting("workspace/reduceMotion", false, { persist: false });
  await select(RELEASE);
  const target = activity.target()!, scroller = document.querySelector<HTMLDivElement>(".act-scroll")!;
  const nativeWheel = new URLSearchParams(location.search).get("phase4-native-wheel") === "1";
  const words = "A wrapping Markdown paragraph with **emphasis** and `inline code`. ";
  const markdown = (index: number) => `## Mixed reply ${index}\n\n` + (words.repeat(5 + index % 11)
    + "\n\n- First item\n- Second item\n\n```rust\nfn example() {\n    println!(\"mixed layout\");\n}\n```\n\n| Name | Description |\n| --- | --- |\n| Short | A table cell with wrapping words |\n\n").repeat(1 + index % 4);
  let seq = 1, marker = "BOTTOM LAST MARKER";
  const files = (index: number) => [{ request_id: `req-bottom-${index}`, index: 0, reference: "[Image #1]", name: "late-image.png", mime: "image/png", bytes: 68 }];
  const events: ActivityEvent[] = Array.from({ length: 100 }, (_, index) => ({ seq: seq++,
    type: index % 5 === 4 ? "UserPromptSubmit" : "AgentMessage", source: index % 5 === 4 ? "hgs_delivery" : "claude_transcript",
    attachments: index % 5 === 4 ? files(index) : undefined, message_id: `bottom-${index}`, at: 1793000000 + index,
    detail: markdown(index) + (index === 99 ? marker : "") }));
  activity.applyActivity([{ computer: target.computer, key: target.key, kind: "replace", payload: { events } },
    { computer: target.computer, key: target.key, kind: "state", payload: { rev: Number.MAX_SAFE_INTEGER,
      value: { provider_messages: [], message_events: [], conversation_id: target.conversationId } } }]);
  await until(() => scroller.querySelector<HTMLElement>(".act-col")?.style.visibility === "visible" && scroller.scrollHeight > 20000,
    15000, "mixed Activity did not place");
  await sleep(700);
  const failures: string[] = [];
  const geometry = (phase: string, atEnd = true) => {
    const edge = scroller.getBoundingClientRect().top + scroller.clientTop;
    const row = [...scroller.querySelectorAll<HTMLElement>(".act-virtual-row")].at(-1), rect = row?.getBoundingClientRect();
    const gap = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    const visible = !!rect && rect.bottom > edge && rect.bottom <= edge + scroller.clientHeight + 1;
    const value = { phase, top: scroller.scrollTop, height: scroller.scrollHeight, viewport: scroller.clientHeight, gap,
      lastIndex: row?.dataset.index, lastBottom: rect ? rect.bottom - edge : null, visible,
      marker: !!row?.textContent?.includes(marker), rows: scroller.querySelectorAll(".act-virtual-row").length };
    log(`phase4-bottom ${JSON.stringify(value)}`);
    if (atEnd && (gap > 1.5 || !visible || !value.marker)) failures.push(`${phase}:gap=${gap},last=${value.lastIndex}`);
    return value;
  };
  const move = (top: number) => {
    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -100, bubbles: true }));
    scroller.scrollTo({ top, behavior: "instant" });
  };
  const end = async (phase: string) => {
    scroller.dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true }));
    scroller.scrollTo({ top: scroller.scrollHeight, behavior: "instant" });
    await sleep(700); geometry(phase);
  };
  geometry("initial");
  if (!document.querySelector(".act-queue") || !editor()) throw new Error("queue/composer missing from the bottom fixture");

  // Reproduce one intermediate range swap: remove an overscan row before its
  // spacer is updated and force layout, then restore it in the same task.
  // A virtual sizer must preserve the extent even at this intermediate read.
  move(scroller.scrollHeight - scroller.clientHeight - 100);
  await sleep(60);
  const overscan = scroller.querySelector<HTMLElement>(".act-virtual-row")!;
  const before = { top: scroller.scrollTop, height: scroller.scrollHeight };
  const display = overscan.style.display;
  overscan.style.display = "none";
  const interim = { top: scroller.scrollTop, height: scroller.scrollHeight };
  overscan.style.display = display;
  const restored = { top: scroller.scrollTop, height: scroller.scrollHeight };
  log(`phase4-bottom-range-swap ${JSON.stringify({ before, interim, restored })}`);
  if (Math.abs(restored.top - before.top) > 1) failures.push("range swap clamped native scrollTop");
  await sleep(400);

  if (nativeWheel) {
    move(scroller.scrollHeight - scroller.clientHeight - 5000);
    await sleep(500);
    let writes = 0;
    const original = scroller.scrollTo;
    scroller.scrollTo = function (...args: Parameters<HTMLElement["scrollTo"]>) { writes++; return original.apply(scroller, args); } as HTMLElement["scrollTo"];
    const box = scroller.getBoundingClientRect();
    log(`phase4-native-wheel-ready ${JSON.stringify({ bottomReach: true, x: Math.round(box.left + box.width * .7), y: Math.round(box.top + box.height * .5) })}`);
    try {
      await until(() => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= 1.5, 30000, "native wheel never reached scroll max");
    } catch (error) {
      geometry("wheel-timeout"); log(`phase4-bottom-writes ${writes}`); throw error;
    } finally { log("phase4-native-wheel-complete"); scroller.scrollTo = original; }
    await sleep(900); geometry("trusted-wheel");
    if (writes) failures.push(`application scroll writes during native wheel:${writes}`);
    move(scroller.scrollHeight / 3); await sleep(450);
    scroller.focus({ preventScroll: true }); log("phase4-native-end-ready");
    await until(() => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= 1.5, 10000, "native End never reached scroll max");
    await sleep(700); geometry("trusted-end");
  } else await end("scrollbar-max");

  const append = () => {
    marker = `BOTTOM APPEND ${seq}`;
    const event: ActivityEvent = { seq: seq++, type: "UserPromptSubmit", source: "hgs_delivery", at: 1793001000 + seq,
      message_id: `bottom-append-${seq}`, detail: markdown(seq) + marker, attachments: files(seq) };
    activity.applyActivity([{ computer: target.computer, key: target.key, kind: "events", payload: { events: [event], cursor: event.seq } }]);
  };
  move(scroller.scrollHeight / 3); await sleep(450);
  const anchor = activityReadingRows(scroller).visible()[0];
  for (let index = 0; index < 3; index++) { append(); await sleep(60); }
  await sleep(500);
  const offset = anchor && activityReadingRows(scroller).offset(anchor.key);
  if (offset === undefined || Math.abs(offset - anchor.offset) > 1) failures.push("streaming below moved the reading anchor");
  await end("after-reading-appends");
  for (let index = 0; index < 3; index++) { append(); await sleep(120); }
  await sleep(700); geometry("streaming-pinned");
  await until(() => {
    const last = [...scroller.querySelectorAll<HTMLElement>(".act-virtual-row")].at(-1);
    const image = last?.querySelector<HTMLImageElement>(".att-thumb img");
    return !!image && image.complete && image.naturalWidth === 1;
  }, 10000, "late attachment did not load");
  geometry("late-attachment");
  const body = scroller.querySelector<HTMLElement>(".act-virtual-row:last-child .act-body")!;
  const late = document.createElement("pre"); late.className = "act-code"; body.append(late);
  await sleep(50); late.textContent = "Late code layout after measurement\n".repeat(32);
  await sleep(700); geometry("late-content");
  const size = { width: innerWidth, height: innerHeight };
  const column = scroller.querySelector<HTMLElement>(".act-col")!;
  let font: FontFace | undefined;
  try {
    await invoke("perf_resize", { width: 950, height: 700 }); await sleep(700); geometry("resize");
    await end("resize-max");
    font = new FontFace("Activity Bottom Smoke", 'local("Noto Sans Mono")');
    document.fonts.add(font); column.style.fontFamily = '"Activity Bottom Smoke"';
    await font.load(); await sleep(900); geometry("late-font"); await end("font-max");
  } finally {
    column.style.fontFamily = ""; if (font) document.fonts.delete(font);
    await invoke("perf_resize", size);
  }
  await sleep(800); geometry("restored-size");
  if (failures.length) throw new Error(`bottom reachability failed: ${failures.join("; ")}`);
}

async function coldStep() {
  await select(PAGINATION);
  type("cold cache smoke");
  await composer.send();
  await until(() => document.querySelector("#composer-dlg-title")?.textContent === "Continue with a cold cache?", 5000, "the cold-cache question did not open");
  button("Send with full context", document.querySelector("dialog[open]")!).click();
  const sent = await waitCall("send", 2, 15_000);
  if (!sent[1].stdin.includes("cold cache smoke")) throw new Error("the full-context send has the wrong payload");
}

async function startStep() {
  const row = await select(REVIEW);
  type("resume smoke");
  await composer.send();
  await until(() => document.querySelector("#composer-dlg-title")?.textContent === "Start the session to send?", 5000, "the start question did not open");
  button("Resume", document.querySelector("dialog[open]")!).click();
  await waitCall("resume", 1);
  await invoke("smoke_resume", { key: row.catalogKey });
  await until(() => !!composer.view()?.waiting || composer.view()?.local.some((row) => row.status === "sent") === true, 3000, "the waiting state did not appear");
  const sent = await waitCall("send", 3, 20_000)
    .catch((error) => {
      const view = composer.view();
      throw new Error(`${error.message}; ${JSON.stringify({ status: view?.status, waiting: view?.waiting, block: view?.block_reason,
        local: view?.local.map((row) => row.status), dialog: !!document.querySelector("dialog.dlg[open]"), notices: notices().map((notice) => notice.text) })}`);
    });
  if (!sent[2].stdin.includes("resume smoke")) throw new Error("the resumed send has the wrong payload");
}

async function resumeHistoryStep() {
  const row = await select(REVIEW);
  const historical = () => [...document.querySelectorAll<HTMLElement>(".act-virtual-row [id^=item-]")]
    .find((node) => node.textContent?.includes("Existing Activity reply before resume"));
  await until(() => !!historical() && getComputedStyle(document.querySelector(".act-col")!).visibility === "visible", 5000, "the stopped history did not appear");
  const original = historical()!;
  const follow = bridge.activityPort.follow;
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  let held = false;
  const requests: unknown[] = [];
  bridge.activityPort.follow = async (request) => {
    requests.push(request);
    if (request.name === REVIEW && request.run_id?.endsWith("-resumed")) { held = true; await gate; }
    return follow(request);
  };
  try {
    await perform(row, { kind: "resume" });
    await waitCall("resume", 1);
    await invoke("smoke_resume", { key: row.catalogKey });
    await until(() => held, 5000, "the resumed Activity did not subscribe").catch((error) => {
      throw new Error(`${error.message}: ${JSON.stringify({ selected: selectedKey(), target: activity.target(), requests,
        original: { key: row.key, run: row.runId, conversation: row.conversationId, state: row.state, archive: row.archiveId },
        rows: orderedRows().filter((next) => next.name === REVIEW).map((next) => ({ key: next.key, run: next.runId, conversation: next.conversationId, state: next.state, archive: next.archiveId })) })}`);
    });
    for (let frame = 0; frame < 12; frame++) {
      await new Promise(requestAnimationFrame);
      if (!original.isConnected || getComputedStyle(document.querySelector(".act-col")!).visibility !== "visible")
        throw new Error("the visible history disappeared while the new run's page was pending");
    }
    release();
    await until(() => activity.summary()?.run_id === `${row.runId}-resumed`, 5000, "the new run did not settle");
    if (historical() !== original) throw new Error("resuming replaced the existing message DOM");
  } finally {
    release();
    bridge.activityPort.follow = follow;
  }
}

async function startPollStep() {
  const row = await select(REVIEW);
  type("resume without switching views");
  await composer.send();
  await until(() => document.querySelector("#composer-dlg-title")?.textContent === "Start the session to send?", 5000, "the start question did not open");
  button("Compact and resume", document.querySelector("dialog[open]")!).click();
  await waitCall("resume", 1);
  // Publish a starting stream frame, while the CLI's next inspection is ready.
  await invoke("smoke_resume", { key: row.catalogKey });
  await waitCall("compact-context", 1, 15_000);
  const sent = await waitCall("send", 1, 15_000);
  await until(() => !composer.view()?.waiting && !composer.view()?.sending, 3000, "the queued send did not settle");
  const message = JSON.parse(sent[0].stdin);
  if (message.text !== "resume without switching views" || !message.expected_run_id.endsWith("-resumed") || !message.expected_compaction_id)
    throw new Error("the resumed send lost its original message or ready run/compaction identity");
  await sleep(3000);
  if ((await calls("send")).length !== 1 || (await calls("compact-context")).length !== 1)
    throw new Error("the queued continuation repeated an effect");
  if (detailTab() !== "activity" || (await calls("a")).length !== 0)
    throw new Error("the queued send needed a Terminal view switch");
}

async function resumeStatusStep() {
  const row = await select(REVIEW);
  const open = composer.open;
  const follow = bridge.activityPort.follow;
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  let held = false, early = false, released = false;
  composer.open = async (session) => {
    if (session?.key.endsWith("-resumed")) { held = true; await gate; }
    return open(session);
  };
  bridge.activityPort.follow = (request) => {
    if (request.name === REVIEW && request.run_id?.endsWith("-resumed") && !released) early = true;
    return follow(request);
  };
  try {
    await perform(row, { kind: "resume" });
    await waitCall("resume", 1);
    await invoke("smoke_resume", { key: row.catalogKey });
    await until(() => held, 5000, "the resumed composer did not open");
    // Under the old ordering, ensure the sole ready frame has actually been
    // consumed while native still follows the old key before releasing open.
    if (early) await until(() => activity.summary()?.run_id === `${row.runId}-resumed`, 5000, "the early frame did not arrive");
    released = true;
    release();
    await until(() => activity.summary()?.run_id === `${row.runId}-resumed` && composer.view()?.loaded === true
      && composer.view()?.block_reason === null && composer.view()?.session_blocks_input === false,
    7000, "the no-message composer stayed starting after resume").catch((error) => {
      const view = composer.view();
      throw new Error(`${error.message}: ${JSON.stringify({ early, status: view?.session_status, block: view?.block_reason })}`);
    });
    if (early) throw new Error("the first new-run frame raced ahead of the native composer binding");
    if ((await calls("send")).length || (await calls("compact-context")).length || (await calls("a")).length || detailTab() !== "activity")
      throw new Error("resuming without a message needed an effect or a Terminal switch");
  } finally {
    release();
    composer.open = open;
    bridge.activityPort.follow = follow;
  }
}

async function processesStep() {
  await select(API);
  await setSetting("processes/enabled", true);
  document.querySelector<HTMLElement>(".scard.selected")!.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 400, clientY: 300 }));
  await until(() => [...document.querySelectorAll<HTMLButtonElement>("button")].some((candidate) => candidate.textContent?.trim() === "Show running commands"), 3000, "the background-job menu entry did not appear");
  const commands = button("Show running commands");
  if (commands.disabled) throw new Error("the background-job menu entry stayed disabled with live jobs and Processes on");
  commands.click();
  await until(() => processes.view()?.groups.length === 3, 15_000, "the menu did not open the process groups");
  await until(() => flag("processes/enabled") && processes.view()?.groups.length === 3, 15_000, "the three process groups did not appear")
    .catch((error) => {
      const view = processes.view();
      throw new Error(`${error.message}; ${JSON.stringify({ enabled: flag("processes/enabled"), selected: selectedKey(), summary: view?.summary,
        failed: view?.failed, groups: view?.groups.map((group) => [group.group, group.rows.length]) })}`);
    });
  processes.click("call-git-fetch", { toggle: false, range: false });
  await until(() => processes.view()?.detail?.output?.includes("running 42 tests") === true, 10_000, "the process output did not arrive");
  await setSetting("processes/enabled", false);
  setDetailTab("activity");
}

async function previewStep() {
  const row = await select(API);
  await until(() => [...document.querySelectorAll(".att-card")].some((card) => card.textContent?.includes("orders.json")), 10_000, "the JSON attachment card is missing");
  const fetched = await bridge.attachmentFetch(row.computer, { name: row.name, request_id: "req-fixture-1", index: 0,
    conversation_id: row.conversationId ?? "", archive_id: null, agent_id: null });
  if (fetched.mime !== "application/json" || fetched.name !== "orders.json") throw new Error("the JSON attachment read is wrong");
  await until(() => {
    const image = document.querySelector<HTMLImageElement>(".att-card .att-thumb img");
    return !!image && image.src.startsWith("zerus-file:") && image.complete && image.naturalWidth === 1;
  }, 10_000, "the convertFileSrc thumbnail did not decode");
  await until(() => {
    const message = [...document.querySelectorAll<HTMLElement>(".act-msg")].find((row) => row.textContent?.includes("Reconciled image message"));
    const image = message?.querySelector<HTMLImageElement>(".att-card .att-thumb img");
    return !!message && !message.textContent?.includes("Inspect the attached image")
      && !!image && image.src.startsWith("zerus-file:") && image.complete && image.naturalWidth === 1;
  }, 10_000, "the native image echo did not become a decoded attachment card");
  // This read can finish after the successful image's independent read.
  // Keep the same raw-text/no-card assertion, after its result settles.
  await until(() => {
    const missing = [...document.querySelectorAll<HTMLElement>(".act-msg")].find((row) => row.textContent?.includes("Unreadable image stays raw"));
    return !!missing?.textContent?.includes("Inspect the attached image") && !missing.querySelector(".att-card");
  }, 10_000, "an unreadable native image echo did not keep its raw text");
  const reads = await calls("attachment");
  for (const request of ["req-fixture-echo-image", "req-fixture-missing-image"]) {
    if (!reads.some((entry) => entry.argv.includes(request))) throw new Error(`the echo did not use its guarded read: ${request}`);
  }
}

async function questionStep() {
  const row = await select(DOCS);
  await until(() => document.querySelector(".qcard-title")?.textContent === "Agent needs your answer" && composer.view()?.input_visible === false,
    10_000, "the required question card did not hide the editor");
  const card = document.querySelector(".qcard")!;
  const unit = [...card.querySelectorAll<HTMLElement>("label.qchoice")].find((choice) => choice.textContent?.trim() === "Unit");
  if (!unit) throw new Error("the Unit option is missing");
  const input = unit.querySelector("input")!;
  await until(() => !input.disabled, 10_000, "the Unit option stayed disabled");
  input.click();
  await until(() => input.checked, 3000, "the Unit option was not selected");
  await until(() => !button("Submit answer", card).disabled, 5000, "Submit stayed disabled");
  button("Submit answer", card).click();
  const sent = await waitCall("answer", 1);
  const request = JSON.parse(sent[0].stdin) as AnswerRequest;
  if (request.question_id !== "tool:fixture-1" || request.expected_question_hash !== QUESTION_HASH
    || request.expected_run_id !== row.runId || request.expected_conversation_id !== row.conversationId) {
    throw new Error("the answer has the wrong question identity");
  }
  if (request.answers.length !== 1 || request.answers[0].question_id !== "q_0"
    || request.answers[0].selected_option_ids.join(",") !== "opt_0_0" || request.answers[0].text !== "") {
    throw new Error("the answer has the wrong selected option");
  }
  await until(() => document.querySelector(".qcard-status")?.textContent === "Answer sent. Waiting for the agent…", 5000, "the answer status did not appear");
}

async function approvalStep() {
  await select(FLAKY);
  await until(() => !!document.querySelector(".qcard"), 10_000, "the approval card did not appear");
  questions.setActive(true);
  const card = document.querySelector(".qcard")!;
  await until(() => [...card.querySelectorAll("button")].some((candidate) => candidate.textContent?.trim() === "Approve once (3)"), 3000, "the review countdown did not start");
  if (!button("Approve once (3)", card).disabled) throw new Error("Approve once was enabled during the review");
  const approve = () => [...card.querySelectorAll<HTMLButtonElement>("button")].find((candidate) => candidate.textContent?.trim() === "Approve once");
  await until(() => !!approve() && !approve()!.disabled, 5000, "Approve once stayed disabled after the review");
  approve()!.click();
  const sent = await waitCall("answer", 2);
  const request = JSON.parse(sent[1].stdin) as AnswerRequest;
  if (request.answers.length !== 1 || request.answers[0].selected_option_ids.join(",") !== "allow") {
    throw new Error("the approval has the wrong decision");
  }
}

async function queueStep() {
  await select(RELEASE);
  await until(() => document.querySelector(".act-queue")?.textContent?.includes("also update the changelog") === true,
    10_000, "the native queue did not appear");
  const panel = document.querySelector(".act-queue")!;
  await until(() => panel.querySelector(".act-body b")?.textContent === "Queued review"
    && panel.querySelector(".att-caption")?.textContent?.includes("pasted-image.png") === true,
  5000, "the queue did not use the outgoing Markdown and receipt attachment renderer");
  if (panel.textContent?.includes("Inspect the attached") || panel.textContent?.includes("↳")
    || panel.textContent?.includes("/synthetic/")) throw new Error("raw terminal queue chrome/path leaked into the outgoing preview");
  if (getComputedStyle(panel).borderTopStyle !== "dashed" || !panel.querySelector(".act-queue-head svg"))
    throw new Error("queued messages are missing their pending treatment");
  await until(() => {
    const image = panel.querySelector<HTMLImageElement>(".att-thumb img");
    return !!image && image.complete && image.naturalWidth === 1;
  }, 5000, "the exact queued receipt thumbnail did not load");
  if (button("Send now", panel).disabled) throw new Error("Send now is disabled");
  button("Send now", panel).click();
  const sent = await waitCall("send-now", 1);
  const request = JSON.parse(sent[0].stdin) as QueueRequest;
  if (request.queue_id !== "queue-fixture") throw new Error("Send now names the wrong queue");
  await until(() => notices().some((notice) => notice.text === "Send now requested. Waiting for the agent to accept the queue."),
    5000, "the Send now notice did not appear");
}

async function imagePasteStep() {
  await select(API);
  const bytes = Uint8Array.from(atob(PNG), (char) => char.charCodeAt(0));
  await composer.pasteImage(new Blob([bytes], { type: "image/png" }));
  await until(() => composer.view()?.attachments.some((file) => file.reference === "[Image #1]") === true, 10_000, "the pasted image did not attach");
  button("1 attached").click();
  await until(() => {
    const image = document.querySelector<HTMLImageElement>(".att-row .att-thumb img");
    return !!image && image.src.startsWith("zerus-file:") && image.complete && image.naturalWidth === 1;
  }, 10_000, "the draft thumbnail did not decode");
  document.querySelector(".att-pop")?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
}

async function clearCloseOrderStep() {
  await select(API);
  composer.openClear();
  await until(() => document.querySelector("#composer-dlg-title")?.textContent === "Clear session?", 3000, "the Clear dialog did not open");
  button("Clear session", document.querySelector("dialog[open]")!).click();
  await until(() => !document.querySelector("dialog.dlg[open]"), 1000, "the Clear dialog stayed open");
  await until(() => document.querySelector(".toasts")?.textContent?.includes("Clearing session") === true, 1000, "the Clear Undo did not appear");
  await until(() => composer.view()?.clear.running === true, 10_000, "the clear did not start after its grace period");
  if (document.querySelector("dialog.dlg[open]")) throw new Error("the Clear dialog reopened while the command ran");
  await waitCall("clear-context", 1);
  await until(() => composer.view()?.clear.running === false, 5000, "the clear did not finish");
}

async function openChild(agentId: string, label: string) {
  const expand = [...document.querySelectorAll<HTMLButtonElement>("button")].find((candidate) => candidate.title === "Show subagents");
  expand?.click();
  await until(() => [...document.querySelectorAll<HTMLElement>(".rowhit")].some((row) => row.textContent?.includes(label)), 10_000, `${label} is not in the roster`);
  const row = [...document.querySelectorAll<HTMLElement>(".rowhit")].find((candidate) => candidate.textContent?.includes(label))!;
  row.click();
  await until(() => subagentOpen() === agentId && subagentActivity.target()?.agentId === agentId && subagentActivity.loaded()
    && composer.view()?.subagent?.agent_id === agentId && composer.view()?.loaded === true,
    15_000, `${label} did not open its own Activity and composer`);
}

async function subagentStep() {
  const parent = await select(API);
  await openChild("agent-writable", "Writable child");
  await until(() => composer.view()?.subagent?.send_supported === true && composer.view()?.input_visible === true, 10_000, "the writable child composer did not open");
  type("child smoke");
  await until(() => composer.text() === "child smoke", 1000, "the child editor did not take the text");
  button("Main activity").click();
  await until(() => !subagentOpen() && composer.view()?.key === `${parent.computer}\n${parent.name}`, 5000, "the parent composer did not return");
  await openChild("agent-writable", "Writable child");
  if (composer.text() !== "child smoke") throw new Error("the child draft did not survive navigation");
  await until(() => composer.sendEnabled(), 10_000, "the reopened child did not restore Send");
  const before = (await calls("send")).length;
  await composer.send();
  await until(() => composer.view()?.local.some((row) => row.status === "sent" && row.text === "child smoke") === true, 10_000, "the child send did not settle");
  const sent = await waitCall("send", before + 1);
  const request = JSON.parse(sent[before].stdin) as { expected_run_id: string; expected_conversation_id: string; agent_id?: string; text: string };
  if (request.expected_run_id !== parent.runId || request.expected_conversation_id !== parent.conversationId
    || request.agent_id !== "agent-writable" || request.text !== "child smoke") {
    throw new Error(`the child send has the wrong identity: ${sent[before].stdin}`);
  }
  await openChild("agent-readonly", "Read-only child");
  await until(() => document.querySelector(".subagent-bar")?.textContent?.includes("Read only. This agent is managed by the parent conversation.") === true,
    10_000, "the read-only child hint did not arrive");
  if (composer.view()?.input_visible !== false || document.querySelector<HTMLElement>(".cmp-actions")?.hidden !== true) {
    throw new Error("the read-only child composer is not hidden");
  }
  await composer.send();
  await sleep(50);
  if ((await calls("send")).length !== before + 1) throw new Error("the read-only child sent a message");
  button("Main activity").click();
  await until(() => !subagentOpen(), 5000, "the child view did not close");
}

async function hideLaunchStatusStep() {
  const admission = await bridge.creationSubmit({
    host: "", machine: "workstation", cmd: "claude", account: "", tag: "hide-launch-smoke", group: "ungrouped", group_name: "Inbox",
    implicit_inbox: true, swarm: "", target: "/home/example/src/sample-project", source: "fixture", auto: false, branch: "main", catalog: null,
    add_folder: false, model: "", effort: "", terminal: false, repository: false, clone_done: true, acquisition_confirmed: true,
  });
  if (!("admitted" in admission)) throw new Error(`the synthetic launch was not admitted: ${JSON.stringify(admission)}`);
  const id = admission.admitted;
  document.querySelector<HTMLButtonElement>('button[data-tip="Session creation status"]')!.click();
  const choice = [...document.querySelectorAll<HTMLButtonElement>("button")].find((candidate) => candidate.textContent?.trim().startsWith("hide-launch-smoke:"));
  if (!choice) throw new Error("the launch is missing from the creation status menu");
  choice.click();
  await until(() => [...document.querySelectorAll<HTMLElement>(".rowhit")].some((row) => row.textContent?.includes("hide-launch-smoke")), 10_000, "the launch card is missing");
  const row = [...document.querySelectorAll<HTMLElement>(".rowhit")].find((candidate) => candidate.textContent?.includes("hide-launch-smoke"))!;
  row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 400, clientY: 300 }));
  button("Hide launch status").click();
  await untilAsync(async () => (await bridge.creationsView()).find((card) => card.id === id)?.show_pending === false, 5000, "the launch status was not hidden");
  if (await bridge.creationDetails(id) === null) throw new Error("hiding the launch discarded its operation");
  if ([...document.querySelectorAll<HTMLElement>(".rowhit")].some((candidate) => candidate.textContent?.includes("hide-launch-smoke"))) {
    throw new Error("the hidden launch row is still visible");
  }
}

async function unfocusedMotionStep() {
  const root = document.documentElement;
  const saved = new Map(["pageHidden", "motionUnfocused", "reduceMotion"].map((key) => [key, root.dataset[key]]));
  const fixture = document.createElement("div");
  fixture.style.cssText = "position:fixed;left:600px;top:200px;width:350px;z-index:100";
  try {
    delete root.dataset.pageHidden;
    delete root.dataset.reduceMotion;
    root.dataset.motionUnfocused = "1";
    fixture.innerHTML = '<div class="qcard arrived">A question arriving while unfocused</div><div class="toast">An arriving notice</div><span class="typing"><i></i></span>';
    document.body.append(fixture);
    const question = fixture.querySelector<HTMLElement>(".qcard")!;
    const toast = fixture.querySelector<HTMLElement>(".toast")!;
    const endless = fixture.querySelector<HTMLElement>(".typing i")!;
    const entrances = [...question.getAnimations(), ...toast.getAnimations()];
    if (entrances.length < 2) throw new Error("the question/toast entrance fixture has no animations");
    if (getComputedStyle(endless).animationPlayState !== "paused") throw new Error("endless motion did not pause while unfocused");
    await until(() => entrances.every((animation) => animation.playState === "finished"), 2500,
      "a visible question/toast entrance froze while unfocused");
    if (Number(getComputedStyle(question).opacity) < .99 || Number(getComputedStyle(toast).opacity) < .99) {
      throw new Error("a completed entrance stayed invisible while unfocused");
    }
    root.dataset.pageHidden = "1";
    const hidden = document.createElement("div"); hidden.className = "qcard arrived"; fixture.append(hidden);
    if (getComputedStyle(hidden).animationPlayState !== "paused") throw new Error("hidden-document entrances must pause");
  } finally {
    fixture.remove();
    for (const [key, value] of saved) {
      if (value === undefined) delete root.dataset[key]; else root.dataset[key] = value;
    }
  }
}

export async function phase4Smoke(allowed: boolean, smokeFile?: string) {
  const exit = (code: number) => invoke("exit_app", { code }).catch(() => {});
  if (!allowed) {
    log("phase4-smoke not started: it needs ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_HGS and ZERUS_CLIENT_PERF");
    return;
  }
  const steps: [string, () => Promise<void>][] = [
    ["terminal", terminalStep],
    ["saved-drafts-dialog", () => savedDraftsDialogStep()],
    ["drafts", draftsStep],
    ["saved-drafts-go-to", () => savedDraftsDialogStep(true)],
    ["attach", () => attachStep(smokeFile!)],
    ["send", () => sendStep(smokeFile!)],
    ["cold", coldStep],
    ["start", startStep],
    ["start-poll", startPollStep],
    ["resume-history", resumeHistoryStep],
    ["resume-status", resumeStatusStep],
    ["processes", processesStep],
    ["preview", previewStep],
    ["question", questionStep],
    ["approval", approvalStep],
    ["queue", queueStep],
    ["image-paste", imagePasteStep],
    ["clear-close-order", clearCloseOrderStep],
    ["send-follow", sendFollowStep],
    ["subagent", subagentStep],
    ["hide-launch-status", hideLaunchStatusStep],
    ["unfocused-motion", unfocusedMotionStep],
    ["activity-anchor", activityAnchorStep],
    ["reading-append", readingAppendStep],
    ["active-scroll", activeScrollStep],
    ["variable-scroll", variableScrollStep],
    ["long-selection-copy", longSelectionCopyStep],
    ["select-all-copy", selectAllCopyStep],
    ["media-playback", mediaPlaybackStep],
    ["activity-geometry", activityGeometryStep],
    ["activity-bottom", activityBottomStep],
  ];
  let current = "start";
  try {
    if (!smokeFile) throw new Error("ZERUS_CLIENT_SMOKE_FILE is not set");
    const only = new URLSearchParams(location.search).get("phase4-only");
    if (only && !steps.some(([name]) => name === only)) throw new Error("Unknown phase4 step");
    // These steps need separate fixture setups and run individually.
    for (const [name, run] of steps.filter(([name]) => only ? name === only : !["start-poll", "resume-history", "resume-status"].includes(name))) {
      current = name;
      await run();
      log(`phase4-smoke ${name}: done`);
    }
    await exit(0);
  } catch (error) {
    log(`phase4-smoke ${current}: failed (${String(error)})`);
    await exit(1);
  }
}
