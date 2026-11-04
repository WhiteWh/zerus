import { describe, expect, it, vi } from "vitest";
import {
  ACTIVITY_WIDTH_KEY, PROJECT_SPLIT_KEY, SESSIONS_WIDTH_KEY, DEFAULTS, HINT_KEY, OVERVIEW_WIDTH_KEY, STORAGE_KEY, createSettings, readHint, sameType,
  storageBackend, takeLegacy, writeHint, type Saved, type SettingValue, type Settings, type SettingsBackend,
  type SettingsStorage,
} from "./settings";

describe("imported machine appearance settings", () => {
  it("loads dynamic color and vivid keys and refuses malformed overrides", async () => {
    const backend = storageBackend(memoryStorage({ [STORAGE_KEY]: JSON.stringify({
      "machines/colors/peer": "#abcdef", "machines/vivid/peer": true,
      "machines/colors/bad": "not-a-color", "machines/vivid/bad": "true",
      "machines/colors/": "#123456", "machines/colors/a/b": "#123456",
    }) }));
    expect((await backend.load()).values).toEqual({ "machines/colors/peer": "#abcdef", "machines/vivid/peer": true });
  });

  it("saves exact machine keys and removes only the color on Automatic", async () => {
    const storage = memoryStorage();
    const backend = storageBackend(storage);
    await backend.save("machines/colors/peer", "#abcdef");
    await backend.save("machines/vivid/peer", true);
    await backend.save("machines/colors/other", "#123456");
    expect(await backend.save("machines/colors/peer", null)).toEqual({ value: null, revision: 4 });
    expect((await backend.load()).values).toEqual({ "machines/vivid/peer": true, "machines/colors/other": "#123456" });
  });
});

function memoryStorage(initial: Record<string, string> = {}): SettingsStorage & { data: Record<string, string> } {
  const data = { ...initial };
  return {
    data,
    getItem: (key) => (key in data ? data[key] : null),
    setItem: (key, value) => { data[key] = value; },
    removeItem: (key) => { delete data[key]; },
  };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/**
 * A core stand-in whose saves and load answer when the test says: each save
 * takes the next revision when it is answered, as the core's lock orders them.
 */
function fakeCore(values: Settings = {}) {
  let revision = 0;
  const saves: { key: string; value: SettingValue | null; answer: (refuse?: boolean) => void }[] = [];
  let listener: ((key: string, change: Saved) => void) | undefined;
  let answerLoad: (() => void) | undefined;
  let deferLoad = false;
  let failLoad = false;
  const store: Settings = { ...DEFAULTS, ...values };
  const backend: SettingsBackend = {
    load: () => new Promise((resolve, reject) => {
      const answer = () => (failLoad ? reject("Settings are not readable") : resolve({ values: { ...store }, revision }));
      if (deferLoad) answerLoad = answer; else answer();
    }),
    save: (key, value) => new Promise((resolve, reject) => {
      saves.push({
        key, value, answer: (refuse) => {
          if (refuse) { reject("Settings are not readable"); return; }
          const clamped = key === "processes/activeSeconds" && typeof value === "number" ? Math.max(1, Math.min(60, value)) : value;
          if (clamped === null) delete store[key]; else store[key] = clamped;
          const change = { value: clamped, revision: ++revision };
          // The core announces the change before the command answers.
          listener?.(key, change);
          resolve(change);
        },
      });
    }),
    listen: async (onChange) => { listener = onChange; return () => { listener = undefined; }; },
  };
  return {
    backend, saves, store,
    /** A change made elsewhere, taking the next revision. */
    elsewhere: (key: string, value: SettingValue | null) => {
      if (value === null) delete store[key]; else store[key] = value;
      listener?.(key, { value, revision: ++revision });
    },
    /** An event that arrives late, with an older revision. */
    late: (key: string, value: SettingValue, at: number) => listener?.(key, { value, revision: at }),
    deferLoad: () => { deferLoad = true; },
    failLoad: () => { failLoad = true; },
    answerLoad: () => answerLoad?.(),
  };
}

describe("defaults", () => {
  it("are Qt's, by QSettings name, from the core's table", () => {
    expect(DEFAULTS["workspace/animateActivity"]).toBe(true);
    expect(DEFAULTS["workspace/theme"]).toBe("system");
    expect(DEFAULTS["processes/activeSeconds"]).toBe(2.5);
    expect(DEFAULTS["client/darkSkin"]).toBe("zerus-dark");
    expect(DEFAULTS["automaticApprovals/enabled"]).toBe(false);
  });
  it("accept only values of the default's type", () => {
    expect(sameType("workspace/compact", true)).toBe(true);
    expect(sameType("workspace/compact", "true")).toBeUndefined();
    expect(sameType("no/such", true)).toBeUndefined();
  });
});

describe("answers and events in any order", () => {
  it("keeps one save in flight per key and sends only the newest value next", async () => {
    const core = fakeCore();
    const store = createSettings(core.backend);
    await store.ready;
    void store.set("workspace/contentScale", 1.1);
    void store.set("workspace/contentScale", 1.2);
    void store.set("workspace/contentScale", 1.3);
    expect(core.saves.map((s) => s.value)).toEqual([1.1]);
    // The first answer does not replace the newer value on screen.
    core.saves[0].answer();
    await flush();
    expect(store.settings["workspace/contentScale"]).toBe(1.3);
    expect(core.saves.map((s) => s.value)).toEqual([1.1, 1.3]);
    core.saves[1].answer();
    await flush();
    expect(store.settings["workspace/contentScale"]).toBe(1.3);
  });

  it("drops an event older than what the key shows", async () => {
    const core = fakeCore();
    const store = createSettings(core.backend);
    await store.ready;
    void store.set("workspace/theme", "dark");
    core.saves[0].answer();
    await flush();
    void store.set("workspace/theme", "light");
    core.saves[1].answer();
    await flush();
    core.late("workspace/theme", "dark", 1);
    expect(store.settings["workspace/theme"]).toBe("light");
  });

  it("shows the value the store put in effect", async () => {
    const core = fakeCore();
    const store = createSettings(core.backend);
    await store.ready;
    const saving = store.set("processes/activeSeconds", 0.2);
    expect(store.settings["processes/activeSeconds"]).toBe(0.2);
    core.saves[0].answer();
    expect(await saving).toBe(true);
    expect(store.settings["processes/activeSeconds"]).toBe(1);
  });

  it("reverts a refusal to the store's value, including a change made elsewhere meanwhile", async () => {
    const core = fakeCore({ "workspace/theme": "dark" });
    const store = createSettings(core.backend);
    await store.ready;
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const saving = store.set("workspace/theme", "light");
    core.elsewhere("workspace/theme", "system");
    // While our save is pending the event only updates what the store holds.
    expect(store.settings["workspace/theme"]).toBe("light");
    core.saves[0].answer(true);
    expect(await saving).toBe(false);
    warn.mockRestore();
    expect(store.settings["workspace/theme"]).toBe("system");
  });

  it("ignores a refusal of an older value once a newer one waits", async () => {
    const core = fakeCore({ "workspace/theme": "dark" });
    const store = createSettings(core.backend);
    await store.ready;
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    void store.set("workspace/theme", "light");
    void store.set("workspace/theme", "system");
    core.saves[0].answer(true);
    await flush();
    expect(store.settings["workspace/theme"]).toBe("system");
    core.saves[1].answer();
    await flush();
    warn.mockRestore();
    expect(store.settings["workspace/theme"]).toBe("system");
  });

  it("leaves a tried value alone when an answer or refusal arrives", async () => {
    const core = fakeCore();
    const store = createSettings(core.backend);
    await store.ready;
    void store.set("workspace/animateActivity", false);
    await store.set("workspace/animateActivity", true, { persist: false });
    core.saves[0].answer();
    await flush();
    expect(store.settings["workspace/animateActivity"]).toBe(true);
    store.restore();
    expect(store.settings["workspace/animateActivity"]).toBe(false);
  });

  it("does not let the load undo a change made before it answered", async () => {
    const core = fakeCore({ "workspace/theme": "dark", "workspace/language": "en" });
    core.deferLoad();
    const store = createSettings(core.backend);
    await flush();
    void store.set("workspace/language", "ru");
    core.elsewhere("workspace/theme", "light");
    core.answerLoad();
    await store.ready;
    expect(store.settings["workspace/language"]).toBe("ru");
    expect(store.settings["workspace/theme"]).toBe("light");
  });

  it("removes a shortcut override when the store answers null", async () => {
    const core = fakeCore({ "shortcuts/go.anywhere": ["Ctrl+J"] });
    const store = createSettings(core.backend);
    await store.ready;
    expect(store.settings["shortcuts/go.anywhere"]).toEqual(["Ctrl+J"]);
    void store.set("shortcuts/go.anywhere", null);
    core.saves[0].answer();
    await flush();
    expect("shortcuts/go.anywhere" in store.settings).toBe(false);
  });
});

describe("this window's own saves", () => {
  it("show the stored value when a save made during the load is refused", async () => {
    const core = fakeCore({ "workspace/theme": "dark" });
    core.deferLoad();
    const store = createSettings(core.backend);
    await flush();
    void store.set("workspace/theme", "light");
    core.answerLoad();
    await store.ready;
    expect(store.settings["workspace/theme"]).toBe("light");
    core.saves[0].answer(true);
    await flush();
    expect(store.settings["workspace/theme"]).toBe("dark");
  });

  it("confirm what a fresh store loads at revision 0", async () => {
    const core = fakeCore({ "workspace/theme": "dark" });
    const store = createSettings(core.backend);
    await store.ready;
    void store.set("workspace/theme", "light");
    core.saves[0].answer(true);
    await flush();
    expect(store.settings["workspace/theme"]).toBe("dark");
  });

  it("show the value the store put in effect although its event came first", async () => {
    const core = fakeCore();
    const store = createSettings(core.backend);
    await store.ready;
    void store.set("processes/activeSeconds", 0.2);
    core.saves[0].answer();
    await flush();
    expect(store.settings["processes/activeSeconds"]).toBe(1);
  });

  it("show a newer change made elsewhere while the save was pending", async () => {
    const core = fakeCore({ "workspace/theme": "system" });
    const store = createSettings(core.backend);
    await store.ready;
    void store.set("workspace/theme", "light");
    core.saves[0].answer();
    core.elsewhere("workspace/theme", "dark");
    await flush();
    expect(store.settings["workspace/theme"]).toBe("dark");
    void store.set("workspace/theme", "light");
    core.elsewhere("workspace/theme", "dark");
    core.saves[1].answer();
    await flush();
    // Our answer (newest revision) wins over the earlier change.
    expect(store.settings["workspace/theme"]).toBe("light");
  });

  it("rewrite the first-paint hint after this window's change", async () => {
    const storage = memoryStorage();
    const core = fakeCore();
    const store = createSettings(core.backend, { remember: (s) => writeHint(storage, s) });
    await store.ready;
    void store.set("client/darkSkin", "fjord");
    core.saves[0].answer();
    await flush();
    expect(JSON.parse(storage.data[HINT_KEY])["client/darkSkin"]).toBe("fjord");
  });

  it("drop an event older than the load", async () => {
    const core = fakeCore();
    void core.backend.save("workspace/theme", "dark").catch(() => {});
    core.saves[0].answer();
    const store = createSettings(core.backend);
    await store.ready;
    core.late("workspace/theme", "light", 1);
    expect(store.settings["workspace/theme"]).toBe("dark");
  });
});

describe("the one-time moves", () => {
  it("take the old settings and widths, and clear them only after every save", async () => {
    const storage = memoryStorage({
      [STORAGE_KEY]: JSON.stringify({ "workspace/reduceMotion": true }),
      [OVERVIEW_WIDTH_KEY]: "1480",
      [ACTIVITY_WIDTH_KEY]: "1100",
    });
    const legacy = takeLegacy(storage, true);
    expect(legacy.moves).toEqual({ "workspace/reduceMotion": true, "client/overviewWidth": 1480, "workspace/activityWidth": 1100 });
    const core = fakeCore();
    const store = createSettings(core.backend, { legacy });
    await flush();
    expect(Object.keys(storage.data)).toHaveLength(3);
    for (const save of core.saves) save.answer();
    await store.ready;
    expect(storage.data).toEqual({});
    expect(store.settings["workspace/activityWidth"]).toBe(1100);
  });

  it("moves Next split choices ahead of the Qt seed and retains them on failure", async () => {
    const storage = memoryStorage({ [PROJECT_SPLIT_KEY]: "360", [SESSIONS_WIDTH_KEY]: "420" });
    const legacy = takeLegacy(storage, true);
    expect(legacy.moves).toEqual({ "workspace/projectFolderSplit": 360, "workspace/sessionsWidth": 420 });
    const core = fakeCore({ "workspace/projectFolderSplit": 280, "workspace/sessionsWidth": 310 });
    const store = createSettings(core.backend, { legacy });
    await flush();
    expect(storage.data[PROJECT_SPLIT_KEY]).toBe("360");
    for (const save of core.saves) save.answer();
    await store.ready;
    expect(store.settings["workspace/projectFolderSplit"]).toBe(360);
    expect(store.settings["workspace/sessionsWidth"]).toBe(420);
    expect(storage.data).toEqual({});
  });

  it("keep the old values when a save is refused", async () => {
    const storage = memoryStorage({ [OVERVIEW_WIDTH_KEY]: "1480" });
    const core = fakeCore();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const store = createSettings(core.backend, { legacy: takeLegacy(storage, true) });
    await flush();
    core.saves[0].answer(true);
    await store.ready;
    warn.mockRestore();
    expect(storage.data[OVERVIEW_WIDTH_KEY]).toBe("1480");
  });

  it("move only the widths in the browser dev mode, which keeps its settings object", async () => {
    const storage = memoryStorage({ [STORAGE_KEY]: JSON.stringify({ "workspace/theme": "dark" }), [ACTIVITY_WIDTH_KEY]: "1000" });
    const store = createSettings(storageBackend(storage), { legacy: takeLegacy(storage, false) });
    await store.ready;
    expect(JSON.parse(storage.data[STORAGE_KEY])).toEqual({ "workspace/theme": "dark", "workspace/activityWidth": 1000 });
    expect(storage.data[ACTIVITY_WIDTH_KEY]).toBeUndefined();
    expect(store.settings["workspace/theme"]).toBe("dark");
  });
});

describe("the first-paint hint", () => {
  it("starts from this viewer's copy and refreshes it after load and changes", async () => {
    const storage = memoryStorage({ [HINT_KEY]: JSON.stringify({ "workspace/theme": "dark", "workspace/contentScale": 1.5, other: 1 }) });
    expect(readHint(storage)).toEqual({ "workspace/theme": "dark", "workspace/contentScale": 1.5 });
    const core = fakeCore({ "workspace/theme": "light" });
    const store = createSettings(core.backend, { hint: readHint(storage), remember: (s) => writeHint(storage, s) });
    expect(store.settings["workspace/theme"]).toBe("dark");
    expect(store.settings["workspace/contentScale"]).toBe(1.5);
    await store.ready;
    expect(JSON.parse(storage.data[HINT_KEY])["workspace/theme"]).toBe("light");
    void store.set("workspace/contentScale", 1.25);
    core.saves[0].answer();
    await flush();
    expect(JSON.parse(storage.data[HINT_KEY])["workspace/contentScale"]).toBe(1.25);
  });

  it("keeps a value still being saved out of it", async () => {
    const storage = memoryStorage();
    const core = fakeCore();
    const store = createSettings(core.backend, { remember: (s) => writeHint(storage, s) });
    await store.ready;
    void store.set("workspace/theme", "light");
    core.elsewhere("workspace/contentScale", 1.25);
    const hint = JSON.parse(storage.data[HINT_KEY]);
    expect(hint["workspace/contentScale"]).toBe(1.25);
    expect(hint["workspace/theme"]).toBe("system");
    core.saves[0].answer(true);
    await flush();
    expect(store.settings["workspace/theme"]).toBe("system");
    expect(JSON.parse(storage.data[HINT_KEY])["workspace/theme"]).toBe("system");
  });

  it("stays as it was when the load fails", async () => {
    const before = JSON.stringify({ "workspace/theme": "dark", "workspace/contentScale": 1.5 });
    const storage = memoryStorage({ [HINT_KEY]: before });
    const core = fakeCore();
    core.failLoad();
    const store = createSettings(core.backend, { hint: readHint(storage), remember: (s) => writeHint(storage, s) });
    await store.ready;
    expect(store.loaded()).toBe(true);
    core.elsewhere("workspace/language", "ru");
    core.elsewhere("workspace/contentScale", 1.25);
    expect(storage.data[HINT_KEY]).toBe(before);
  });

  it("is not written from defaults before the load", async () => {
    const before = JSON.stringify({ "workspace/theme": "dark" });
    const storage = memoryStorage({ [HINT_KEY]: before });
    const core = fakeCore({ "workspace/theme": "dark" });
    core.deferLoad();
    const store = createSettings(core.backend, { hint: readHint(storage), remember: (s) => writeHint(storage, s) });
    await flush();
    core.elsewhere("workspace/contentScale", 1.25);
    expect(storage.data[HINT_KEY]).toBe(before);
    core.answerLoad();
    await store.ready;
    const hint = JSON.parse(storage.data[HINT_KEY]);
    expect(hint["workspace/theme"]).toBe("dark");
    expect(hint["workspace/contentScale"]).toBe(1.25);
  });
});

describe("the browser dev mode", () => {
  it("keeps typed values in local storage", async () => {
    const storage = memoryStorage({ [STORAGE_KEY]: JSON.stringify({ "workspace/theme": "dark", "workspace/compact": "yes" }) });
    const backend = storageBackend(storage);
    expect(await backend.load()).toEqual({ values: { "workspace/theme": "dark" }, revision: 0 });
    expect((await backend.save("workspace/compact", true)).value).toBe(true);
  });
});


describe("local project style", () => {
  it("loads exact flat project IDs including false and rejects malformed keys/values", async () => {
    const backend = storageBackend(memoryStorage({ [STORAGE_KEY]: JSON.stringify({
      "projects/vivid/alpha": false, "projects/vivid/nested/project": true,
      "projects/vivid/": true, "projects/vivid/bad\nidentity": true,
      "projects/vivid/not-a-flag": "true", ["projects/vivid/" + "a".repeat(257)]: true,
    }) }));
    expect((await backend.load()).values).toEqual({ "projects/vivid/alpha": false, "projects/vivid/nested/project": true });
  });
});
