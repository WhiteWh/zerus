import { describe, expect, it, vi } from "vitest";
import { createRoot } from "solid-js";

// The module's app instance reads the page's stores, which read the page's query and storage.
vi.hoisted(() => {
  const store = new Map<string, string>();
  vi.stubGlobal("location", new URL("http://localhost/"));
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
});

import { COMPOSER_HINT, TERMINAL_HINT, createDrops, routeDrop, type DropDeps, type DropState } from "./drops";

const state = (change: Partial<DropState> = {}): DropState => ({
  selected: "\nrun:a", online: true, tab: "activity", subagent: false,
  terminal: { connected: false, generation: 0 }, composer: { key: "\nclaude/sample-project/review", canAttach: true }, ...change,
});
const terminalOn = { tab: "terminal" as const, terminal: { connected: true, generation: 4 } };

describe("where a drop goes (SessionsWindow.cpp:1595-1601, 2618-2622)", () => {
  it("never attaches Task View files to the session agent's draft", () => {
    expect(routeDrop(state({ tab: "taskview" }))).toEqual({ kind: "none" });
  });
  it("goes to the Terminal while it shows, connected to an online session", () => {
    expect(routeDrop(state(terminalOn))).toEqual({ kind: "terminal", key: "\nrun:a\n4" });
    expect(routeDrop(state({ ...terminalOn, online: false }))).toEqual({ kind: "none" });
    expect(routeDrop(state({ tab: "terminal", terminal: { connected: false, generation: 0 } }))).toEqual({ kind: "none" });
  });

  it("goes to the composer on any other tab while it can attach, and switches to Activity there", () => {
    for (const tab of ["activity", "processes", "native"] as const) {
      expect(routeDrop(state({ tab }))).toEqual({ kind: "composer", key: "\nclaude/sample-project/review" });
    }
    expect(routeDrop(state({ composer: { key: "\nclaude/sample-project/review", canAttach: false } }))).toEqual({ kind: "none" });
    expect(routeDrop(state({ composer: { key: undefined, canAttach: false } }))).toEqual({ kind: "none" });
  });

  it("uses the current child composer or takes nothing without a selected session", () => {
    expect(routeDrop(state({ subagent: true }))).toEqual({ kind: "composer", key: "\nclaude/sample-project/review" });
    expect(routeDrop(state({ subagent: true, composer: { key: "\nclaude/sample-project/review", canAttach: false } }))).toEqual({ kind: "none" });
    expect(routeDrop(state({ selected: undefined }))).toEqual({ kind: "none" });
  });
});

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

function harness(change: Partial<DropDeps> = {}) {
  const calls: string[] = [];
  let now = state();
  let panel = true;
  let files = true;
  const deps: DropDeps = {
    state: () => now,
    overPanel: () => panel,
    files: async (paths) => { calls.push(`files ${paths.join(",")}`); return files; },
    reveal: () => { calls.push("reveal"); },
    attach: async (paths) => { calls.push(`attach ${paths.join(",")}`); },
    peer: () => undefined,
    terminalDrop: async (generation, paths) => { calls.push(`terminal ${generation} ${paths.join(",")}`); return null; },
    notify: (tone, text, sticky) => { calls.push(`${tone}${sticky ? " sticky" : ""} ${text}`); },
    ...change,
  };
  const drops = createRoot(() => createDrops(deps));
  return {
    drops, calls,
    set: (s: Partial<DropState>) => { now = state(s); },
    leavePanel: () => { panel = false; },
    folder: () => { files = false; },
  };
}

describe("a drop on the session's panel (SessionFileDrop.h)", () => {
  it("shows what a drop does while over the panel, and attaches to the composer on Activity", async () => {
    const h = harness();
    h.drops.enter(["/srv/home/example/notes.txt"], 10, 10);
    await tick();
    expect(h.drops.hint()).toBe(COMPOSER_HINT);
    await h.drops.drop(["/srv/home/example/notes.txt"], 10, 10);
    expect(h.drops.hint()).toBeUndefined();
    expect(h.calls).toEqual(["files /srv/home/example/notes.txt", "reveal", "attach /srv/home/example/notes.txt"]);
  });

  it("refuses a drop with a folder in it, as Qt takes only files", async () => {
    const h = harness();
    h.folder();
    h.drops.enter(["/srv/home/example"], 10, 10);
    await tick();
    expect(h.drops.hint()).toBeUndefined();
    await h.drops.drop(["/srv/home/example"], 10, 10);
    expect(h.calls).toEqual(["files /srv/home/example"]);
  });

  it("takes nothing dropped outside the panel, and hides the overlay when the drag leaves it", async () => {
    const h = harness();
    h.drops.enter(["/srv/a.txt"], 10, 10);
    await tick();
    h.leavePanel();
    h.drops.over(900, 10);
    expect(h.drops.hint()).toBeUndefined();
    await h.drops.drop(["/srv/a.txt"], 900, 10);
    expect(h.calls).toEqual(["files /srv/a.txt"]);
  });

  it("takes nothing when the target changed between the drag's entry and its drop", async () => {
    const h = harness();
    h.drops.enter(["/srv/a.txt"], 10, 10);
    await tick();
    h.set({ composer: { key: "\nclaude/sample-project/other", canAttach: true } });
    h.drops.over(10, 10);
    expect(h.drops.hint()).toBeUndefined();
    await h.drops.drop(["/srv/a.txt"], 10, 10);
    expect(h.calls).toEqual(["files /srv/a.txt"]);
  });

  it("forgets a drag that left", async () => {
    const h = harness();
    h.drops.enter(["/srv/a.txt"], 10, 10);
    await tick();
    h.drops.leave();
    expect(h.drops.hint()).toBeUndefined();
    await h.drops.drop(["/srv/a.txt"], 10, 10);
    expect(h.calls).toEqual(["files /srv/a.txt"]);
  });

  it("pastes into this computer's Terminal through its attach, with Rust's notice", async () => {
    const h = harness({ terminalDrop: async (generation, paths) => { return `pasted ${generation} ${paths.length}`; } });
    h.set(terminalOn);
    h.drops.enter(["/srv/a.txt", "/srv/b.txt"], 10, 10);
    await tick();
    expect(h.drops.hint()).toBe(TERMINAL_HINT);
    await h.drops.drop(["/srv/a.txt", "/srv/b.txt"], 10, 10);
    expect(h.calls).toEqual(["files /srv/a.txt,/srv/b.txt", "info pasted 4 2"]);
  });

  it("says a peer's transfer starts, and shows a refusal as a warning", async () => {
    const h = harness({ peer: () => "studio", terminalDrop: async () => { throw "Incomplete file transfer"; } });
    h.set(terminalOn);
    h.drops.enter(["/srv/a.txt"], 10, 10);
    await tick();
    await h.drops.drop(["/srv/a.txt"], 10, 10);
    expect(h.calls).toEqual(["files /srv/a.txt", "info Transferring files to studio…", "error sticky Incomplete file transfer"]);
  });

  it("drops into the Terminal attach the drag entered with only", async () => {
    const h = harness();
    h.set(terminalOn);
    h.drops.enter(["/srv/a.txt"], 10, 10);
    await tick();
    // The Terminal reconnected meanwhile: another attach.
    h.set({ tab: "terminal", terminal: { connected: true, generation: 5 } });
    await h.drops.drop(["/srv/a.txt"], 10, 10);
    expect(h.calls).toEqual(["files /srv/a.txt"]);
  });
});

// The drop texts are Qt's (SessionFileDrop.h, SessionsWindow.cpp).
const fs = (await import(/* @vite-ignore */ ["node", "fs"].join(":"))) as { readFileSync(path: URL, encoding: "utf8"): string };
const read = (file: string) => fs.readFileSync(new URL(file, new URL("../../../", import.meta.url)), "utf8");

describe("the drop texts are Qt's", () => {
  it.each([COMPOSER_HINT, TERMINAL_HINT])("SessionFileDrop says %s", (text) => {
    expect(read("tray/src/SessionFileDrop.h")).toContain(`tr("${text}")`);
  });
  it("fills in Qt's transfer notice", () => {
    expect(read("tray/src/SessionsWindow.cpp")).toContain('tr("Transferring files to %1…")');
    expect(read("client/src/store/drops.ts")).toContain('tr("Transferring files to %1…", peer)');
  });
});
