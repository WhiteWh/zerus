import { describe, expect, it } from "vitest";
import type { CreatedWorktree } from "../../types";
import { NewWorktree, WorktreePicker, baseKey, branchFolder, sameSessions, snapshotTime, worktreeLabel, type PickerSession } from "./worktree-dialogs";

// Choose existing… (tray/src/WorktreePanel.h, picker mode) and Create
// manually… (tray/src/NewWorktreeDialog.h) over recorded calls that answer
// when the test says. Synthetic computers and paths only.

type Json = Record<string, unknown>;
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function held<T>() {
  const calls: { args: unknown[]; answer(value: T): void; fail(error: string): void }[] = [];
  const call = (...args: unknown[]) => new Promise<T>((resolve, reject) => { calls.push({ args, answer: resolve, fail: reject }); });
  return { calls, call };
}

const catalog = (extra: Json = {}): Json => ({
  state: "ok", path: "/srv/repo", selected_root: "/srv/repo", common_dir: "/srv/repo/.git", sampled_at: 0,
  worktrees: [
    { path: "/srv/repo", kind: "main", branch: "main", available: true },
    { path: "/srv/repo-fix", kind: "linked", branch: "fix", available: true, locked: true, locked_reason: "in use" },
    { path: "/srv/repo-old", kind: "linked", detached: true, head: "0123456789abcdef", available: false, prunable: true, prunable_reason: "gone" },
    { path: "/srv/repo.git", kind: "bare", available: true },
  ],
  ...extra,
});

describe("Choose existing… (WorktreePanel in picker mode)", () => {
  function picker(path = "/srv/repo", snapshot: Json = {}) {
    const reads = held<Json>();
    const snapshots: string[] = [];
    const panel = new WorktreePicker({ snapshot: async () => { snapshots.push(path); return snapshot; }, catalog: (refresh) => reads.call(refresh) }, path, "mac");
    return { panel, reads, snapshots };
  }
  const sessions: PickerSession[] = [
    { gitRoot: "/srv/repo", cwdCanonical: "/srv/repo/src" },
    { gitRoot: "/srv/repo-fix", cwdCanonical: "/srv/repo-fix" },
    { gitRoot: "/srv/repo-fix", cwdCanonical: "/srv/repo-fix/app" },
    { gitRoot: "", cwdCanonical: "/srv/notes" },
  ];

  it("labels each checkout with its kind, branch, marks and sessions", () => {
    expect(worktreeLabel({ kind: "main", branch: "main", available: true }, 1)).toBe("Main checkout / main (1 session)");
    expect(worktreeLabel({ kind: "linked", branch: "fix", available: true, locked: true }, 2)).toBe("Linked worktree / fix (locked) (2 sessions)");
    expect(worktreeLabel({ kind: "linked", detached: true, head: "0123456789abcdef", prunable: true }, 0)).toBe("Linked worktree / detached 01234567 (prunable) (unavailable) (0 sessions)");
    expect(worktreeLabel({ kind: "bare", available: true }, 0)).toBe("Bare repository (0 sessions)");
    expect(worktreeLabel({ kind: "folder", available: true }, 3)).toBe("Folder (3 sessions)");
  });

  it("reads the catalog once shown and offers Use folder only for an available, non-bare, current row", async () => {
    const { panel, reads, snapshots } = picker();
    await settle();
    expect(snapshots).toEqual(["/srv/repo"]);
    expect(panel.view(true, sessions, "posix")).toMatchObject({ status: "Git information unavailable. You can still use an accessible folder.", useEnabled: false });
    panel.show(true);
    panel.show(true);
    expect(reads.calls.map((c) => c.args)).toEqual([[false]]);
    expect(panel.view(true, sessions, "posix")).toMatchObject({ status: "Reading folder…", refreshEnabled: false, useEnabled: false });
    reads.calls[0].answer(catalog());
    await settle();
    const view = panel.view(true, sessions, "posix");
    expect(view.status).toBe("Existing working folders on mac");
    expect(view.rows.map((r) => r.label)).toEqual([
      "Main checkout / main (1 session)", "Linked worktree / fix (locked) (2 sessions)",
      "Linked worktree / detached 01234567 (prunable) (unavailable) (0 sessions)", "Bare repository (0 sessions)",
    ]);
    expect(view.rows[1].tooltip).toBe("/srv/repo-fix\nin use\n");
    expect(view).toMatchObject({ selected: "/srv/repo", useEnabled: true, refreshEnabled: true });
    expect(view.rows.map((r) => r.usable)).toEqual([true, true, false, false]);
    panel.select("/srv/repo-old");
    expect(panel.view(true, sessions, "posix").useEnabled).toBe(false);
    panel.select("/srv/repo.git");
    expect(panel.view(true, sessions, "posix").useEnabled).toBe(false);
    panel.select("/srv/repo-fix");
    expect(panel.view(true, sessions, "posix")).toMatchObject({ selected: "/srv/repo-fix", useEnabled: true });
    // Offline, a stale catalog or a read on its way: nothing is current.
    expect(panel.view(false, sessions, "posix")).toMatchObject({ status: "Offline. Showing the last snapshot.", useEnabled: false, refreshEnabled: false });
    panel.refresh(true);
    expect(reads.calls.map((c) => c.args)).toEqual([[false], [true]]);
    expect(panel.view(true, sessions, "posix").useEnabled).toBe(false);
    reads.calls[1].answer(catalog({ stale: true }));
    await settle();
    expect(panel.view(true, sessions, "posix")).toMatchObject({ selected: "/srv/repo-fix", useEnabled: false });
    expect(panel.view(true, sessions, "posix").rows.every((r) => !r.usable)).toBe(true);
  });

  it("words each catalog state and dates its snapshot", async () => {
    const cases: [Json, string][] = [
      [{ state: "not_repo", worktrees: [] }, "Ordinary folder (no Git)."],
      [{ state: "folder_unavailable", worktrees: [] }, "Folder is missing or inaccessible."],
      [{ state: "git_failed", worktrees: [] }, "Git information unavailable. You can still use an accessible folder."],
      [catalog({ partial: true }), "Partial catalog (first 512 worktrees)."],
    ];
    for (const [data, status] of cases) {
      const { panel, reads } = picker();
      panel.show(true);
      reads.calls[0].answer(data);
      await settle();
      expect(panel.view(true, [], "posix").status).toBe(status);
    }
    const sampled = new Date(2026, 9, 5, 7, 3, 9).getTime() / 1000;
    expect(snapshotTime(sampled)).toBe("5 Oct 07:03:09");
    const { panel, reads } = picker("/srv/repo", catalog({ sampled_at: sampled }));
    await settle();
    expect(panel.view(false, [], "posix").status).toBe("Offline. Showing the last snapshot.\nSnapshot: 5 Oct 07:03:09 (outdated)");
    panel.show(true);
    reads.calls[0].fail("ssh failed");
    await settle();
    expect(panel.view(true, [], "posix")).toMatchObject({ status: "Catalog unavailable: ssh failed\nSnapshot: 5 Oct 07:03:09 (outdated)", useEnabled: false });
    panel.refresh(true);
    reads.calls[1].answer(catalog({ sampled_at: sampled }));
    await settle();
    expect(panel.view(true, [], "posix").status).toBe("Existing working folders on mac\nSnapshot: 5 Oct 07:03:09");
    expect(picker("").panel.view(true, [], "posix").status).toBe("Select a folder.");
  });

  it("lists an ordinary folder as one row, counted by its canonical folder", async () => {
    const { panel, reads } = picker("/srv/notes");
    panel.show(true);
    reads.calls[0].answer({ state: "not_repo", path: "/srv/notes", worktrees: [] });
    await settle();
    const view = panel.view(true, sessions, "posix");
    expect(view.rows).toEqual([{ path: "/srv/notes", sessions: 1, label: "Folder (1 session)", tooltip: "/srv/notes\n\n", usable: true }]);
    expect(view.useEnabled).toBe(true);
    // Before any answer the folder is not known to be available.
    expect(picker("/srv/notes").panel.view(true, sessions, "posix").rows[0]).toMatchObject({ label: "Folder (unavailable) (1 session)", usable: false });
    // Windows compares paths with either separator and ignoring case.
    expect(picker("C:/Work").panel.view(true, [{ gitRoot: "", cwdCanonical: "c:\\work" }], "windows").rows[0].label).toBe("Folder (unavailable) (1 session)");
  });

  it("counts sessions once per new data, never on a selection change (WorktreePanel::render)", async () => {
    const { panel, reads } = picker();
    panel.show(true);
    reads.calls[0].answer(catalog());
    await settle();
    // Each session's paths count their reads: one pass over the list per count.
    let reads_ = 0;
    const counted = (gitRoot: string, cwdCanonical: string): PickerSession => ({
      get gitRoot() { reads_++; return gitRoot; },
      get cwdCanonical() { reads_++; return cwdCanonical; },
    });
    const many = [...Array.from({ length: 500 }, (_, i) => counted("/srv/repo", `/srv/repo/s${i}`)), counted("/srv/repo-fix", "/srv/repo-fix")];
    const first = panel.view(true, many, "posix");
    expect(first.rows.map((r) => r.label)).toEqual([
      "Main checkout / main (500 sessions)", "Linked worktree / fix (locked) (1 session)",
      "Linked worktree / detached 01234567 (prunable) (unavailable) (0 sessions)", "Bare repository (0 sessions)",
    ]);
    expect(reads_).toBe(2 * many.length);
    // Selecting, and viewing again with the same data and sessions, reads no session and keeps the rows.
    panel.select("/srv/repo-fix");
    panel.select("/srv/repo");
    const again = panel.view(true, many, "posix");
    expect(reads_).toBe(2 * many.length);
    expect(again.rows).toBe(first.rows);
    expect(again.selected).toBe("/srv/repo");
    // An offline computer marks the rows unusable without counting again.
    expect(panel.view(false, many, "posix").rows.every((r) => !r.usable)).toBe(true);
    expect(reads_).toBe(2 * many.length);
    // New sessions count again, once.
    const fewer = many.slice(0, 10);
    expect(panel.view(true, fewer, "posix").rows[0].label).toBe("Main checkout / main (10 sessions)");
    expect(reads_).toBe(2 * many.length + 2 * fewer.length);
    panel.view(true, fewer, "posix");
    expect(reads_).toBe(2 * many.length + 2 * fewer.length);
    // New data builds the rows again from the same counts.
    panel.refresh(true);
    reads.calls[1].answer(catalog({ worktrees: [{ path: "/srv/repo", kind: "main", branch: "main", available: true }] }));
    await settle();
    expect(panel.view(true, fewer, "posix").rows.map((r) => r.label)).toEqual(["Main checkout / main (10 sessions)"]);
    expect(reads_).toBe(2 * many.length + 2 * fewer.length);
  });

  it("keeps an equal session list as it is, so the picker keeps its counts", () => {
    const one = [{ gitRoot: "/srv/repo", cwdCanonical: "/srv/repo/src" }];
    expect(sameSessions(one, [{ gitRoot: "/srv/repo", cwdCanonical: "/srv/repo/src" }])).toBe(true);
    expect(sameSessions(one, [{ gitRoot: "/srv/repo", cwdCanonical: "/srv/repo" }])).toBe(false);
    expect(sameSessions(one, [...one, ...one])).toBe(false);
    expect(sameSessions([], [])).toBe(true);
  });

  it("never reads an offline computer's catalog, and drops a closed picker's answers", async () => {
    const { panel, reads } = picker();
    panel.show(false);
    panel.refresh(false);
    expect(reads.calls).toEqual([]);
    panel.show(true);
    panel.dispose();
    reads.calls[0].answer(catalog());
    await settle();
    expect(panel.view(true, [], "posix").status).toBe("Reading folder…");
  });

  it("keeps the snapshot from replacing a catalog that answered first", async () => {
    let release!: (value: Json) => void;
    const reads = held<Json>();
    const panel = new WorktreePicker({ snapshot: () => new Promise((resolve) => { release = resolve; }), catalog: (refresh) => reads.call(refresh) }, "/srv/repo", "mac");
    panel.show(true);
    reads.calls[0].answer(catalog());
    await settle();
    release({ state: "not_repo", worktrees: [] });
    await settle();
    expect(panel.view(true, [], "posix").status).toBe("Existing working folders on mac");
  });
});

describe("Create manually… (NewWorktreeDialog)", () => {
  function dialog(options: { catalog?: Json; branch?: string; style?: "posix" | "windows"; source?: string } = {}) {
    const creates = held<CreatedWorktree>();
    const created: CreatedWorktree[] = [];
    const form = new NewWorktree((branch, base, destination) => creates.call(branch, base, destination), "mac", options.source ?? "/srv/repo",
      options.catalog ?? catalog(), options.branch ?? "work-1a2b3c4d", options.style ?? "posix");
    form.onCreated = (value) => created.push(value);
    return { form, creates, created };
  }

  it("suggests the folder beside the repository until it is edited", () => {
    const { form } = dialog();
    expect(form.view()).toMatchObject({
      context: "Computer: mac\nRepository: /srv/repo", branch: "work-1a2b3c4d", base: "HEAD", bases: ["HEAD", "main", "fix"],
      destination: "/srv/repo-work-1a2b3c4d", error: "", busy: false, createEnabled: true,
    });
    form.setBranch("  feature/my task!ü ");
    expect(form.view().destination).toBe("/srv/repo-feature-my-task-ü");
    expect(branchFolder("a//b..c")).toBe("a-b..c");
    form.setDestination("/elsewhere/mine");
    form.setBranch("other");
    expect(form.view().destination).toBe("/elsewhere/mine");
    expect(form.browseStart()).toBe("/elsewhere");
    // Parent folder… follows the branch again under the chosen folder.
    form.browsed("/data");
    expect(form.view().destination).toBe("/data/repo-other");
    form.browsed("/");
    expect(form.view().destination).toBe("/repo-other");
  });

  it("names the selected root's parent and repository, in the computer's style", () => {
    expect(dialog({ catalog: { ...catalog(), selected_root: "/srv/repo-fix" } }).form.view().destination).toBe("/srv/repo-fix-work-1a2b3c4d");
    expect(dialog({ catalog: { state: "ok", worktrees: [] }, source: "/srv/plain" }).form.view()).toMatchObject({ destination: "/srv/plain-work-1a2b3c4d", bases: ["HEAD"] });
    const windows = dialog({ catalog: { state: "ok", selected_root: "C:\\work\\app", worktrees: [] }, style: "windows" }).form;
    expect(windows.view()).toMatchObject({ destination: "C:/work/app-work-1a2b3c4d", createEnabled: true });
    windows.browsed("D:/");
    expect(windows.view().destination).toBe("D:/app-work-1a2b3c4d");
    // A browsed folder in the computer's own form (this computer's folder dialog on Windows) keeps one separator.
    windows.browsed("C:\\data\\");
    expect(windows.view().destination).toBe("C:/data/app-work-1a2b3c4d");
    windows.browsed("D:\\");
    expect(windows.view().destination).toBe("D:/app-work-1a2b3c4d");
    windows.browsed("\\\\server\\share\\work");
    expect(windows.view().destination).toBe("//server/share/work/app-work-1a2b3c4d");
    expect(windows.browseStart()).toBe("//server/share/work");
    // A POSIX path is not a Windows computer's folder, and the reverse.
    windows.setDestination("/srv/app");
    expect(windows.view().createEnabled).toBe(false);
    const posix = dialog().form;
    posix.setDestination("C:\\work\\app");
    expect(posix.view().createEnabled).toBe(false);
  });

  it("steps Start from as QComboBox does: from the current item, with the arrows and Page keys", () => {
    const press = (key: string, extra: Partial<KeyboardEvent> = {}) => ({ key, altKey: false, ctrlKey: false, metaKey: false, shiftKey: false, ...extra });
    expect(baseKey(press("ArrowDown", { altKey: true }))).toBe("open");
    expect(baseKey(press("F4"))).toBe("open");
    expect(baseKey(press("F4", { shiftKey: true }))).toBe("text");
    expect(baseKey(press("ArrowUp"))).toBe("previous");
    expect(baseKey(press("ArrowUp", { altKey: true }))).toBe("previous");
    expect(baseKey(press("PageUp"))).toBe("previous");
    expect(baseKey(press("ArrowDown"))).toBe("next");
    expect(baseKey(press("PageDown"))).toBe("next");
    expect(baseKey(press("PageDown", { ctrlKey: true }))).toBe("next");
    // Control (Command on macOS) with an arrow goes to the text, as do Home, End and letters.
    for (const extra of [{ ctrlKey: true }, { metaKey: true }]) {
      expect(baseKey(press("ArrowUp", extra))).toBe("text");
      expect(baseKey(press("ArrowDown", extra))).toBe("text");
    }
    expect(baseKey(press("Home"))).toBe("text");
    expect(baseKey(press("End"))).toBe("text");
    expect(baseKey(press("a"))).toBe("text");
    expect(baseKey(press("ArrowDown", { keyCode: 229 }))).toBe("text");

    const { form } = dialog();
    expect(form.view()).toMatchObject({ base: "HEAD", bases: ["HEAD", "main", "fix"], baseIndex: 0 });
    form.stepBase(-1);
    expect(form.view()).toMatchObject({ base: "HEAD", baseIndex: 0 });
    form.stepBase(1);
    form.stepBase(1);
    expect(form.view()).toMatchObject({ base: "fix", baseIndex: 2 });
    form.stepBase(1);
    expect(form.view()).toMatchObject({ base: "fix", baseIndex: 2 });
    // Typed text that names an item is the current one.
    form.setBase("main");
    expect(form.view().baseIndex).toBe(1);
    form.stepBase(-1);
    expect(form.view()).toMatchObject({ base: "HEAD", baseIndex: 0 });
    // Typed text that names none keeps the last chosen item current: the step starts there, not at the first.
    form.chooseBase(2);
    form.setBase("origin/feature");
    expect(form.view()).toMatchObject({ base: "origin/feature", baseIndex: 2 });
    form.stepBase(-1);
    expect(form.view()).toMatchObject({ base: "main", baseIndex: 1 });
    form.setBase("origin/feature");
    form.stepBase(1);
    expect(form.view()).toMatchObject({ base: "fix", baseIndex: 2 });
    // At the end the text stays as typed.
    form.setBase("tag-1");
    form.stepBase(1);
    expect(form.view()).toMatchObject({ base: "tag-1", baseIndex: 2 });
  });

  it("needs a branch, a base and an absolute folder", () => {
    const { form } = dialog();
    form.setBranch("  ");
    expect(form.view().createEnabled).toBe(false);
    form.setBranch("feature");
    form.setBase(" ");
    expect(form.view().createEnabled).toBe(false);
    form.setBase("origin/main");
    form.setDestination("relative/folder");
    expect(form.view().createEnabled).toBe(false);
    form.setDestination("/srv/repo-feature");
    expect(form.view().createEnabled).toBe(true);
  });

  it("creates once, refuses closing meanwhile, and shows a failure without sending again", async () => {
    const { form, creates, created } = dialog();
    form.setBranch(" feature ");
    form.setBase(" HEAD ");
    form.create();
    form.create();
    expect(creates.calls.map((c) => c.args)).toEqual([["feature", "HEAD", "/srv/repo-feature"]]);
    expect(form.view()).toMatchObject({ error: "Creating worktree…", busy: true, createEnabled: false });
    expect(form.canClose()).toBe(false);
    form.setBranch("changed");
    form.setDestination("/other");
    form.browsed("/data");
    expect(form.view()).toMatchObject({ branch: " feature ", destination: "/srv/repo-feature" });
    creates.calls[0].fail("branch exists\nCheck /srv/repo-feature before retrying. Any created worktree is kept.");
    await settle();
    expect(form.view()).toMatchObject({ error: "branch exists\nCheck /srv/repo-feature before retrying. Any created worktree is kept.", busy: false, createEnabled: true });
    expect(form.canClose()).toBe(true);
    expect(creates.calls.length).toBe(1);
    expect(created).toEqual([]);
    form.create();
    const answer = { path: "/srv/repo-feature", catalog: { worktrees: [{ path: "/srv/repo-feature", kind: "linked", available: true }] } };
    creates.calls[1].answer(answer);
    await settle();
    expect(created).toEqual([answer]);
    expect(creates.calls.length).toBe(2);
  });
});
