// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createComponent, ErrorBoundary, Show, Suspense, type Accessor } from "solid-js";
import { render } from "solid-js/web";
const native = vi.hoisted(() => ({ clear: vi.fn(), offer: vi.fn(), cancel: vi.fn(), confirm: vi.fn(), sync: vi.fn(), dismiss: vi.fn(), remove: vi.fn(), inspect: vi.fn(), restore: vi.fn(), manual: vi.fn(), manualCancel: vi.fn(), check: vi.fn() }));
vi.mock("../../bridge", () => ({ inTauri: () => false, terminationCleanupKeep: native.clear, terminationCleanupDismiss: native.dismiss, terminationCleanupRemove: native.remove, terminationCleanupInspect: native.inspect, terminationCleanupManualOffer: native.manual, terminationCleanupManualCancel: native.manualCancel,
  terminationProgramForceOffer: native.offer, terminationProgramForceCancel: native.cancel, terminationProgramForceConfirm: native.confirm, terminationCleanupCheck: native.check }));
const catalogState = vi.hoisted(() => ({ rows: {} as Record<string, { key: string; computer: string; name: string }> }));
const navigation = vi.hoisted(() => ({ select: vi.fn(), tab: vi.fn(), page: vi.fn() }));
vi.mock("../../store/catalog", () => ({ catalog: catalogState }));
vi.mock("../../store/ui", () => ({ setPage: navigation.page, setSelectedKey: navigation.select, setDetailTab: navigation.tab }));
vi.mock("../../store/cleanupNotices", () => ({ anyCleanupRestore: () => false, cleanupRestoring: () => false, restoreCleanupRow: native.restore, syncCleanupNotices: native.sync }));
vi.mock("../../store/actions", () => ({ notify: vi.fn() }));
vi.mock("../../i18n", () => ({ tr: (s: string, ...args: unknown[]) => s.replace(/%(\d+)/g, (_, n) => String(args[Number(n) - 1])), trNoop: (s: string) => s }));
// An empty marker naming the icon: no text, so the copy assertions are unchanged.
vi.mock("../lib/icons", () => ({ Icon: (props: { name: string }) => { const marker = document.createElement("i"); marker.dataset.icon = props.name; return marker; } }));
vi.mock("../components/MachineLabel", () => ({ MachineLabel: () => null }));
const archiveCalls = vi.hoisted(() => ({ calls: [] as string[], disabled: new Set<string>(), fail: false }));
vi.mock("../sessions/ActionDialogs", () => ({ actionEntries: (row: { name: string }) => { if (archiveCalls.fail) throw new Error("Session actions are unavailable."); return { archive: { label: "Move to archive", disabled: archiveCalls.disabled.has(row.name), hint: "Busy", action: () => { archiveCalls.calls.push(row.name); } } }; } }));
import WorktreeCleanup, { CleanupInspection } from "./WorktreeCleanup";
import { CleanupChip, CleanupReviewButton } from "./CleanupChip";
import { applyCleanup, cleanup, cleanupDialog, closeCleanup, emptyCleanup, openCleanup, type CleanupRow, groupCleanupRows } from "../../store/terminationCleanup";
const row = (over: Partial<CleanupRow> = {}): CleanupRow => ({ id: "original", label: "worktree-fixture", path: "/srv/example/work", machine: "this computer", computer: "", branch: "kept-branch", size: "Not measured", status: "kept", reason: "Some programs are still running.", details: "", can_check: false, can_force: true, ready: false, ...over });
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
let revision = 0, dispose: (() => void) | undefined;
beforeEach(() => {
  vi.clearAllMocks(); catalogState.rows = {}; closeCleanup(); applyCleanup({ ...emptyCleanup(), revision: ++revision });
  native.inspect.mockResolvedValue({ path: "unrelated fixture response" });
  native.dismiss.mockImplementation(async () => ({ ...emptyCleanup(), revision: ++revision }));
  native.clear.mockImplementation(async () => ({ ...emptyCleanup(), revision: ++revision }));
  native.offer.mockImplementation(async () => ({ confirmation: "native-handle", remaining_millis: 60_000, revision: cleanup().revision + 1, programs: [{ pid: 42, label: "original-survivor" }] }));
  native.cancel.mockResolvedValue({ ...emptyCleanup(), revision: ++revision });
  vi.spyOn(HTMLDialogElement.prototype, "showModal").mockImplementation(function (this: HTMLDialogElement) { this.open = true; });
  vi.spyOn(HTMLDialogElement.prototype, "close").mockImplementation(function (this: HTMLDialogElement) { this.open = false; });
});
afterEach(() => { dispose?.(); dispose = undefined; closeCleanup(); document.body.replaceChildren(); vi.restoreAllMocks(); });
function mount() {
  const host = document.createElement("div"); document.body.append(host);
  dispose = render(() => [createComponent(CleanupReviewButton, {}), createComponent(CleanupChip, {}), Show({ get when() { return cleanupDialog(); }, children: (_current: Accessor<NonNullable<ReturnType<typeof cleanupDialog>>>) => createComponent(WorktreeCleanup, {}) })], host);
  return host;
}
const button = (host: HTMLElement, text: string) => [...host.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent === text)!;
const review = (host: HTMLElement) => host.querySelector<HTMLButtonElement>("button.cleanup-review")!;

it("keeps the permanent review button and opens a truthful empty state", () => {
  const host = mount(); expect(review(host)).toBeTruthy(); review(host).click();
  expect(host.textContent).toContain("No working folders need review.");
  expect(button(host, "Force stop…")).toBeUndefined();
  button(host, "Close").click(); expect(cleanupDialog()).toBeUndefined();
  review(host).click(); expect(host.querySelector("dialog[open]")).not.toBeNull();
  expect(native.clear).not.toHaveBeenCalled(); expect(native.offer).not.toHaveBeenCalled();
});

it("footer Close repeatedly unmounts and reopens the SAME kept row and Force offer", async () => {
  const original = row(); applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [original], kept: 1 });
  const host = mount();
  for (let i = 0; i < 2; i++) {
    review(host).click(); expect(host.textContent).toContain("worktree-fixture"); expect(button(host, "Force stop…")).toBeTruthy();
    button(host, "Close").click(); expect(native.clear).not.toHaveBeenCalled(); await flush();
    expect(cleanupDialog()).toBeUndefined(); expect(cleanup().rows).toEqual([original]);
    expect(native.clear).not.toHaveBeenCalled();
  }
  review(host).click(); button(host, "Force stop…").click(); await flush();
  expect(native.offer).toHaveBeenCalledExactlyOnceWith("original");
  expect(host.textContent).toContain("original-survivor (PID 42)");
  button(host, "Cancel").click(); await flush(); expect(native.cancel).toHaveBeenCalledExactlyOnceWith("original", "native-handle");
  expect(native.confirm).not.toHaveBeenCalled();
});

it("X and Escape dismiss without clearing, including during background work", () => {
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [row({ busy: true, status: "stopping" })], busy: true });
  const host = mount(); review(host).click();
  host.querySelector<HTMLButtonElement>('button[aria-label="Close"]')!.click();
  expect(cleanupDialog()).toBeUndefined(); review(host).click();
  host.querySelector("dialog")!.dispatchEvent(new Event("cancel", { cancelable: true }));
  expect(cleanupDialog()).toBeUndefined(); expect(cleanup().rows).toHaveLength(1); expect(native.clear).not.toHaveBeenCalled();
});

it("clears only on the explicit action and keeps retained rows and their review open", async () => {
  const retained = row(); applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [retained, row({ id: "settled", can_force: false })], kept: 2, can_clear: true });
  native.clear.mockImplementation(async () => ({ ...emptyCleanup(), revision: ++revision, rows: [retained], kept: 1, can_clear: false }));
  const host = mount(); review(host).click(); button(host, "Clear settled results").click(); await flush();
  expect(native.clear).toHaveBeenCalledTimes(1); expect(cleanup().rows).toEqual([retained]);
  expect(cleanupDialog()).toBeTruthy(); expect(button(host, "Force stop…")).toBeTruthy(); await vi.waitFor(() => expect(native.sync).toHaveBeenCalled());
  expect(button(host, "Clear settled results")).toBeUndefined();
});

it("renders one folder with every source and names the session and program blockers", () => {
  const reason = "Cannot remove this folder while another session is working. " + "Full readable reason. ".repeat(50);
  const sources = ["claude/example/first", "claude/example/second", "claude/example/third"];
  catalogState.rows.live = { key: "run:live", computer: "", name: "claude/example/fourth" };
  const rows = sources.map((name, i) => row({ id: `origin-${i}`, can_force: false, can_dismiss: true, folder_key: "same-native-folder", source: { computer: "", name, run_id: `run-${i}`, conversation_id: `conv-${i}` }, sessions: ["claude/example/fourth"], processes: [{ pid: 42, label: "report-server" }], reason }));
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows, kept: 1, can_clear: true });
  const host = mount(); review(host).click();
  expect(host.querySelectorAll("tbody tr")).toHaveLength(1);
  for (const name of sources) expect(host.textContent).toContain(name);
  expect(host.textContent).toContain("claude/example/fourth");
  expect(host.textContent).toContain("report-server (PID 42)");
  // zerus-l1ryn: the Result cell is the mockup's one line; the full recorded
  // reason stays in its tooltip and the blockers are grouped below the table.
  expect(host.querySelector(".wtc-reason")!.textContent).toBe("Other Zerus sessions use this folder");
  expect(host.querySelector(".wtc-reason")!.classList.contains("ell")).toBe(true);
  expect(host.querySelector<HTMLElement>(".bc-table tbody td:last-child")!.title).toContain(reason);
  button(host, "Open").click();
  expect(navigation.select).toHaveBeenCalledExactlyOnceWith("run:live");
  expect(navigation.page).toHaveBeenCalledWith("sessions");
  expect(native.clear).not.toHaveBeenCalled(); expect(native.confirm).not.toHaveBeenCalled();
});

it("dismisses one unknown result explicitly and never bulk clears or removes it", async () => {
  const unknown = row({ id: "unknown-original", status: "unknown", can_force: false, can_dismiss: true });
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [unknown], unknown: 1, can_clear: false });
  const expectedRevision = revision;
  const host = mount(); review(host).click();
  expect(button(host, "Clear settled results")).toBeUndefined();
  button(host, "Dismiss").click(); await flush();
  expect(native.dismiss).toHaveBeenCalledExactlyOnceWith("unknown-original", expectedRevision);
  expect(native.clear).not.toHaveBeenCalled(); expect(native.remove).not.toHaveBeenCalled();
  expect(native.confirm).not.toHaveBeenCalled();
  expect(host.textContent).toContain("No working folders need review.");
});

it("keeps the pending original Force identity in a grouped folder and offers no Dismiss", async () => {
  const settled = row({ id: "settled", can_force: false, can_dismiss: false, folder_key: "shared" });
  const pending = row({ id: "original-live", can_force: true, can_dismiss: false, folder_key: "shared" });
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [settled, pending], kept: 1, can_clear: false });
  const host = mount(); review(host).click();
  expect(host.querySelectorAll("tbody tr")).toHaveLength(1);
  expect(button(host, "Dismiss")).toBeUndefined();
  button(host, "Force stop…").click(); await flush();
  expect(native.offer).toHaveBeenCalledExactlyOnceWith("original-live");
  expect(native.dismiss).not.toHaveBeenCalled();
  button(host, "Cancel").click(); await flush();
  expect(native.cancel).toHaveBeenCalledWith("original-live", "native-handle");
});

it("reads current blockers without changing the recorded removal decision", async () => {
  const original = row({ can_force: false, can_dismiss: true, sessions: [], processes: [], reason: "The folder is still in use." });
  native.inspect.mockResolvedValue({ path: original.path, common_dir: "/srv/example/.git", review: {sessions: ["claude/example/current"], processes: [{pid: 84, command: "current-report-server"}]} });
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [original], kept: 1 });
  const host = mount(); review(host).click(); await flush();
  expect(host.textContent).toContain("claude/example/current");
  expect(host.textContent).toContain("current-report-server (PID 84)");
  expect(cleanup().rows[0]).toBe(original);
  expect(native.remove).not.toHaveBeenCalled(); expect(native.confirm).not.toHaveBeenCalled();
});

it("preserves raw pending metadata and keeps computers and repositories separate", () => {
  const pending = { ...row({folder_key: "machine-repository-a", can_force: true}), force_revision: 7 };
  const groups = groupCleanupRows([row({id:"settled", folder_key:"machine-repository-a", can_force:false}), pending, row({id:"other-repo", folder_key:"machine-repository-b"}), row({id:"other-computer", computer:"other", folder_key:"other-machine"})]);
  expect(groups).toHaveLength(3);
  expect(groups[0].primary).toBe(pending);
  expect(groups[0].rows[1]).toBe(pending);
  expect((groups[0].primary as typeof pending).force_revision).toBe(7);
});

 it("preserves each archived source restore action in a grouped folder", () => {
  const rows = ["first", "second"].map((name) => {
    const target = {computer: "", name, run_id: name + "-run", conversation_id: name + "-conversation"};
    return row({id: name, can_force: false, folder_key: "shared-folder", source: target, restore: {target, machine: "machine", archive_id: name + "-archive", status: "kept", overflow: false}});
  });
  applyCleanup({...emptyCleanup(), revision: ++revision, rows, kept: 1});
  const host = mount(); review(host).click();
  button(host, "Restore session first").click(); button(host, "Restore session second").click();
  expect(native.restore.mock.calls.map(([row]) => row.id)).toEqual(["first", "second"]);
});

it("never displays a previous computer's blocker during the next same-path inspection", async () => {
  const alpha = row({id:"alpha", computer:"alpha", can_force:false, folder_key:"alpha-folder", sessions:[]});
  const beta = row({id:"beta", computer:"beta", can_force:false, folder_key:"beta-folder", sessions:[]});
  native.inspect.mockImplementation((id:string)=>id==="alpha" ? Promise.resolve({path:alpha.path,review:{sessions:["alpha-only-blocker"]}}) : new Promise(()=>{}));
  applyCleanup({...emptyCleanup(),revision:++revision,rows:[alpha,beta],kept:2});
  const host=mount();review(host).click();await flush();expect(host.textContent).toContain("alpha-only-blocker");
  host.querySelectorAll<HTMLTableRowElement>("tbody tr")[1].click();await flush();
  expect(host.textContent).not.toContain("alpha-only-blocker");
});

it("keeps the original unknown outcome visible beside a later lease retirement", () => {
  applyCleanup({ ...emptyCleanup(), revision: ++revision, unknown: 1, rows: [row({ status: "unknown", can_force: false,
    reason: "Removal could not be confirmed. Check the original result before continuing.", program_retirement: "expired" })] });
  const host = mount(); review(host).click();
  expect(host.textContent).toContain("Removal could not be confirmed");
  expect(host.textContent).toContain("Force stop offer expired");
  expect(cleanup().rows[0].status).toBe("unknown"); expect(button(host, "Force stop…")).toBeUndefined();
  expect(native.confirm).not.toHaveBeenCalled(); expect(native.clear).not.toHaveBeenCalled();
});

it("keeps the pending raw Force ID beside an expired unknown member of the same folder", async () => {
  const expired = row({ id: "old-unknown", folder_key: "shared-folder", status: "unknown", can_force: false,
    reason: "Original removal remains unconfirmed.", program_retirement: "expired" });
  const pending = row({ id: "current-original", folder_key: "shared-folder", can_force: true, can_dismiss: false });
  applyCleanup({ ...emptyCleanup(), revision: ++revision, unknown: 1, rows: [expired, pending] });
  const host = mount(); review(host).click();
  expect(host.querySelectorAll("tbody tr")).toHaveLength(1);
  expect(host.querySelector('[data-outcome="unknown"]')?.textContent).toContain("Original removal remains unconfirmed.");
  expect(host.textContent).toContain("Force stop offer expired");
  button(host, "Force stop…").click(); await flush();
  expect(native.offer).toHaveBeenCalledExactlyOnceWith("current-original");
  expect(cleanup().rows[0]).toBe(expired); expect(cleanup().rows[1]).toBe(pending);
  expect(native.confirm).not.toHaveBeenCalled(); expect(native.dismiss).not.toHaveBeenCalled();
  button(host, "Cancel").click(); await flush();
  expect(native.cancel).toHaveBeenCalledExactlyOnceWith("current-original", "native-handle");
});

it("opens manual removal only through the original native row handle",async()=>{
 native.manual.mockResolvedValue({confirmation:"native-manual",path:"/srv/example/hand-made",computer:"fixture",branch:"kept",size:"0 B",changes:0,untracked:0,ignored:[],required:{local_files:false,discard_changes:false,unchecked_processes:false,merged_branches:false},ready:false,reason:"Current session still uses it",sessions:[],processes:[]});
 applyCleanup({...emptyCleanup(),revision:++revision,rows:[row({id:"unowned-original",can_force:false,can_remove_anyway:true,status:"review"})],kept:1});
 const host=mount();review(host).click();button(host,"Remove anyway…").click();await flush();
 expect(native.manual).toHaveBeenCalledExactlyOnceWith("unowned-original");expect(host.textContent).toContain("/srv/example/hand-made");
 expect(button(host,"Remove working folder").disabled).toBe(true);button(host,"Cancel").click();await flush();expect(native.manualCancel).toHaveBeenCalledExactlyOnceWith("native-manual");
});

it("runs Check again once with no automatic Force or removal", async () => {
 // Matches the native valid-refusal projection exercised by assert_real_refused_check.
 const original = row({ can_check: true, can_force: false, reason: "The program list changed or could not be checked. Review the working folder again." });
 applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [original], kept: 1 });
 native.check.mockResolvedValue({ ...emptyCleanup(), revision: ++revision, rows: [original], kept: 1 });
 const host = mount(); review(host).click();
 button(host, "Check again").click(); await flush();
 expect(native.check).toHaveBeenCalledExactlyOnceWith("original");
 expect(native.offer).not.toHaveBeenCalled();
 expect(native.remove).not.toHaveBeenCalled();
 expect(native.confirm).not.toHaveBeenCalled();
});

it("keeps Check again disabled until the native row admits review", () => {
 const original = row({ can_check: false, can_force: false });
 applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [original], kept: 1 });
 const host = mount(); review(host).click();
 expect(button(host, "Check again").disabled).toBe(true);
 button(host, "Check again").click();
 expect(native.check).not.toHaveBeenCalled();
 expect(native.offer).not.toHaveBeenCalled();
 expect(native.remove).not.toHaveBeenCalled();
});

it("groups a kept folder's blockers and archives other sessions only after a confirmation that lists them (zerus-l1ryn)", async () => {
  archiveCalls.calls = []; archiveCalls.disabled = new Set(["kimi/doge-docs-b"]);
  const names = ["codex/balance-review", "kimi/doge-docs-a", "kimi/doge-docs-b"];
  names.forEach((name, i) => { catalogState.rows[name] = { key: `run:${i}`, computer: "", name }; });
  const kept = row({ can_force: false, can_dismiss: true, sessions: names, processes: [], reason: "Zerus sessions use this folder: " + names.join(", ") + ". Archive them first." });
  native.inspect.mockResolvedValue({ path: kept.path, common_dir: "/srv/example/.git", review: {
    sessions: names, processes: [{ pid: 101, command: "codex" }, { pid: 102, command: "python3" }, { pid: 103, command: "hgs" }], process_count: 3,
    unreadable_processes: [{}, {}, {}, {}, {}, {}, {}], untracked: 1,
    reasons: [{ code: "sessions", message: "Zerus sessions use this folder." }, { code: "processes", message: "3 running processes use this folder." },
      { code: "processes_unavailable", message: "7 processes with hidden paths." }, { code: "untracked", message: "1 untracked file." }] } });
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [kept], kept: 1 });
  const host = mount(); review(host).click(); await flush();
  const panel = host.querySelector(".bc-blockers")!;
  expect(panel.textContent).toContain("Other Zerus sessions use this folder");
  expect(panel.textContent).toContain("Removal waits until these sessions stop using the folder.");
  expect([...panel.querySelectorAll(".bc-group-title")].map((h) => h.textContent)).toEqual(["Other Zerus sessions use this folder", "Running programs", "Untracked files"]);
  for (const text of ["codex (PID 101)", "python3 (PID 102)", "hgs (PID 103)", "Programs Zerus cannot check: 7.", "Untracked files: 1"]) expect(panel.textContent).toContain(text);
  // Nothing is archived before the confirmation, and Cancel archives nothing.
  button(host, "Archive these 3 sessions…").click(); await flush();
  const confirmation = document.querySelector<HTMLDialogElement>("dialog.bc-archive")!;
  await vi.waitFor(() => { for (const name of names) expect(confirmation.textContent).toContain(name); });
  expect(archiveCalls.calls).toEqual([]);
  button(confirmation as unknown as HTMLElement, "Cancel").click(); await flush();
  expect(document.querySelector("dialog.bc-archive")).toBeNull(); expect(archiveCalls.calls).toEqual([]);
  // Confirm runs the menu's own archive once per archivable session only.
  button(host, "Archive these 3 sessions…").click(); await flush();
  const again = document.querySelector<HTMLElement>("dialog.bc-archive")!;
  await vi.waitFor(() => expect(again.textContent).toContain("Busy"));
  button(again, "Archive 2 sessions").click(); await flush();
  expect(archiveCalls.calls).toEqual(["codex/balance-review", "kimi/doge-docs-a"]);
  expect(host.textContent).toContain("Archive requested for 2 sessions.");
  expect(native.remove).not.toHaveBeenCalled(); expect(native.check).not.toHaveBeenCalled(); expect(native.confirm).not.toHaveBeenCalled();
});

it("offers no blocker action for an unknown removal result (zerus-l1ryn)", async () => {
  archiveCalls.calls = [];
  const unknown = row({ status: "unknown", can_force: false, can_dismiss: true, sessions: ["codex/balance-review"], processes: [{ pid: 7, label: "codex" }], reason: "Removal could not be confirmed." });
  catalogState.rows.live = { key: "run:live", computer: "", name: "codex/balance-review" };
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [unknown], unknown: 1 });
  const host = mount(); review(host).click(); await flush();
  const panel = host.querySelector(".bc-blockers")!;
  expect(panel.textContent).toContain("The removal result is unknown and will not be retried.");
  expect(panel.querySelector("button")).toBeNull();
  expect(button(host, "Check again").disabled).toBe(true);
  expect(archiveCalls.calls).toEqual([]); expect(native.remove).not.toHaveBeenCalled();
});

// App.tsx renders the review inside a Suspense boundary; these tests mount it the same way.
function mountSuspended() {
  const host = document.createElement("div"); document.body.append(host);
  dispose = render(() => [createComponent(CleanupReviewButton, {}), Show({ get when() { return cleanupDialog(); }, children: (_current: Accessor<NonNullable<ReturnType<typeof cleanupDialog>>>) =>
    createComponent(Suspense, { get children() { return createComponent(WorktreeCleanup, {}); } }) })], host);
  return host;
}
function deferred<T>() { let resolve!: (value: T) => void, reject!: (error: unknown) => void; const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; }); return { promise, resolve, reject }; }
const keptFolder = (id: string, over: Partial<CleanupRow> = {}) => row({ id, folder_key: id, label: `folder-${id}`, path: `/srv/example/${id}`, can_force: false, can_check: true, can_dismiss: true, processes: [], reason: "Uncommitted changes need review.", ...over });
const live = (host: HTMLElement) => host.querySelector(".bc-live")?.textContent ?? "";
const select = async (host: HTMLElement, id: string) => { host.querySelector<HTMLTableRowElement>(`tr[title="/srv/example/${id}"]`)!.click(); await flush(); };
const reviewOf = (path: string, sessions: string[]) => ({ path, common_dir: "/srv/example/.git", review: { sessions, processes: [], reasons: [{ code: "sessions", message: "Zerus sessions use this folder." }] } });

it("keeps the review visible and usable while the selected row's blocker inspection loads (zerus-nq26a)", async () => {
  const pending = deferred<unknown>(); native.inspect.mockReturnValue(pending.promise);
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [keptFolder("a", { processes: [{ pid: 7, label: "codex" }] }), keptFolder("b")], kept: 2 });
  const host = mountSuspended(); review(host).click(); await flush();
  expect(host.querySelector("dialog.worktree-cleanup[open]")).not.toBeNull();
  expect(button(host, "Check again").disabled).toBe(false);
  expect(live(host)).toContain("Checking what uses this folder…");
  // The recorded blockers stay visible while the fresh read is pending.
  expect(host.querySelector(".bc-blockers")!.textContent).toContain("codex (PID 7)");
  await select(host, "b");
  expect(host.querySelector("dialog.worktree-cleanup[open]")).not.toBeNull();
  expect(host.querySelector("tr.on")?.getAttribute("title")).toBe("/srv/example/b");
  expect(live(host)).toContain("Checking what uses this folder…");
  // One read at a time: the second row waits for the first read to settle.
  expect(native.inspect).toHaveBeenCalledTimes(1); expect(native.inspect).toHaveBeenCalledWith("a");
});

it("never lets a late inspection of another row overwrite the selected row (zerus-nq26a)", async () => {
  const first = deferred<unknown>(), second = deferred<unknown>();
  native.inspect.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [keptFolder("a"), keptFolder("b")], kept: 2 });
  const host = mountSuspended(); review(host).click(); await flush();
  await select(host, "b");
  first.resolve(reviewOf("/srv/example/a", ["codex/late-a"])); await flush();
  expect(host.querySelector(".bc-blockers")?.textContent ?? "").not.toContain("codex/late-a");
  expect(live(host)).toContain("Checking what uses this folder…");
  // The selected row's own read starts only after the late one settled.
  expect(native.inspect).toHaveBeenCalledTimes(2); expect(native.inspect).toHaveBeenLastCalledWith("b");
  second.resolve(reviewOf("/srv/example/b", ["codex/fresh-b"])); await flush();
  expect(host.querySelector(".bc-blockers")!.textContent).toContain("codex/fresh-b");
  expect(host.querySelector(".bc-blockers")!.textContent).not.toContain("codex/late-a");
  expect(live(host)).toBe("");
  // Going back to the first row reads it again rather than reusing the late reply.
  await select(host, "a");
  expect(native.inspect).toHaveBeenCalledTimes(3); expect(native.inspect).toHaveBeenLastCalledWith("a");
});

it("shows the recorded blockers and a failure note when the inspection fails (zerus-nq26a)", async () => {
  native.inspect.mockRejectedValue("A folder inspection is already running. Try again after it finishes.");
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [keptFolder("a", { processes: [{ pid: 7, label: "codex" }] })], kept: 1 });
  const host = mountSuspended(); review(host).click(); await flush();
  expect(host.querySelector("dialog.worktree-cleanup[open]")).not.toBeNull();
  expect(host.querySelector(".bc-blockers")!.textContent).toContain("codex (PID 7)");
  expect(live(host)).toContain("Could not check what uses this folder now. The recorded result is shown.");
  expect(button(host, "Check again").disabled).toBe(false);
  expect(native.remove).not.toHaveBeenCalled(); expect(native.check).not.toHaveBeenCalled();
});

it("ignores a reply that arrives after the review closed (zerus-nq26a)", async () => {
  const pending = deferred<unknown>();
  native.inspect.mockReturnValueOnce(pending.promise).mockResolvedValueOnce(reviewOf("/srv/example/a", ["codex/reopened"]));
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [keptFolder("a"), keptFolder("b")], kept: 2 });
  const host = mountSuspended(); review(host).click(); await flush();
  await select(host, "b"); // b waits behind a's read
  button(host, "Close").click(); await flush(); expect(cleanupDialog()).toBeUndefined();
  pending.resolve(reviewOf("/srv/example/a", ["codex/after-close"])); await flush();
  expect(host.querySelector("dialog.worktree-cleanup")).toBeNull();
  // A closed review starts no further read.
  expect(native.inspect).toHaveBeenCalledTimes(1);
  // Reopening reads afresh; the closed review's reply never reaches it.
  review(host).click(); await flush();
  expect(native.inspect).toHaveBeenCalledTimes(2); expect(native.inspect).toHaveBeenLastCalledWith("a");
  expect(host.querySelector(".bc-blockers")!.textContent).toContain("codex/reopened");
  expect(host.querySelector(".bc-blockers")!.textContent).not.toContain("codex/after-close");
});

it("keeps the inspection visible while its local file page loads (zerus-nq26a)", async () => {
  const page = deferred<unknown>();
  const read = vi.fn((_id: string, file?: string) => file ? page.promise : Promise.resolve({ path: "/srv/example/a", files: [], changes: { text: "" }, commits: { text: "" } }));
  const host = document.createElement("div"); document.body.append(host);
  dispose = render(() => createComponent(Suspense, { get children() { return createComponent(CleanupInspection, { row: keptFolder("a"), close: () => {}, read: read as never }); } }), host);
  await flush(); expect(host.querySelector("dialog.worktree-inspection")).not.toBeNull();
  button(host, "Local").click(); await flush();
  expect(read).toHaveBeenLastCalledWith("a", ".", 0);
  expect(host.querySelector("dialog.worktree-inspection")).not.toBeNull();
  expect(button(host, "Next").disabled).toBe(true);
  page.resolve({ path: "/srv/example/a", file: { path: ".", entries: [{ path: "notes.txt", directory: false, symlink: false }], offset: 0 } }); await flush();
  expect(host.textContent).toContain("notes.txt");
});

it("treats a reply for another path as a failed check, never as this folder's blockers (zerus-nq26a)", async () => {
  native.inspect.mockResolvedValue(reviewOf("/srv/example/elsewhere", ["codex/elsewhere"]));
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [keptFolder("a")], kept: 1 });
  const host = mountSuspended(); review(host).click(); await flush();
  expect(host.querySelector(".bc-blockers")!.textContent).not.toContain("codex/elsewhere");
  expect(live(host)).toContain("Could not check what uses this folder now.");
});

// The reviewer's probe: a MutationObserver records any moment the review leaves the DOM, even if it comes back.
function detachProbe(host: HTMLElement) {
  const review = host.querySelector("dialog.worktree-cleanup")!; expect(review).not.toBeNull();
  // Records-based: a removal that is undone a microtask later still counts.
  let detached = false;
  const scan = (records: MutationRecord[]) => { for (const record of records) for (const node of record.removedNodes) if (node === review || node.contains(review)) detached = true; };
  const observer = new MutationObserver(scan);
  observer.observe(host, { childList: true, subtree: true });
  return { detached: () => { scan(observer.takeRecords()); return detached || !review.isConnected; }, stop: () => observer.disconnect() };
}

it("keeps the review attached while the Remove anyway offer is pending (zerus-nq26a.2)", async () => {
  const offer = deferred<unknown>(); native.manual.mockReturnValue(offer.promise);
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [keptFolder("a", { can_remove_anyway: true })], kept: 1 });
  const host = mountSuspended(); review(host).click(); await flush();
  const probe = detachProbe(host);
  button(host, "Remove anyway…").click(); await flush();
  expect(probe.detached()).toBe(false);
  const manual = document.querySelector<HTMLElement>("dialog[aria-labelledby=manual-remove-title]")!;
  expect(manual.textContent).toContain("Checking the folder…");
  expect(button(manual, "Remove working folder").disabled).toBe(true);
  expect(probe.detached()).toBe(false);
  offer.resolve({ confirmation: "native-manual", path: "/srv/example/a", computer: "this computer", branch: "kept-branch", size: "1 KB", changes: 0, untracked: 0, reason: "Not created by Zerus.", sessions: [], processes: [], ready: false,
    required: { local_files: false, discard_changes: false, unchecked_processes: false, merged_branches: false } });
  await flush();
  expect(manual.textContent).toContain("Not created by Zerus.");
  expect(button(manual, "Remove working folder").disabled).toBe(true);
  expect(probe.detached()).toBe(false); probe.stop();
  expect(native.remove).not.toHaveBeenCalled();
});

it("keeps the review attached while the archive confirmation loads its sessions (zerus-nq26a.2)", async () => {
  archiveCalls.calls = []; archiveCalls.disabled = new Set();
  const names = ["codex/one", "codex/two"];
  names.forEach((name, i) => { catalogState.rows[name] = { key: `run:${i}`, computer: "", name }; });
  native.inspect.mockResolvedValue(reviewOf("/srv/example/a", names));
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [keptFolder("a", { sessions: names })], kept: 1 });
  const host = mountSuspended(); review(host).click(); await flush();
  const probe = detachProbe(host);
  button(host, "Archive these 2 sessions…").click();
  // Same tick as the click: the review is still attached and the confirmation is loading.
  expect(probe.detached()).toBe(false);
  const confirmation = document.querySelector<HTMLElement>("dialog.bc-archive")!;
  expect(confirmation).not.toBeNull();
  expect(button(confirmation, "Archive 2 sessions").disabled).toBe(true);
  await vi.waitFor(() => expect(confirmation.textContent).toContain("codex/two"));
  expect(button(confirmation, "Archive 2 sessions").disabled).toBe(false);
  expect(probe.detached()).toBe(false); probe.stop();
  expect(archiveCalls.calls).toEqual([]);
});

it("shows why the archive confirmation cannot list sessions instead of throwing (zerus-nq26a.2)", async () => {
  archiveCalls.calls = []; archiveCalls.disabled = new Set(); archiveCalls.fail = true;
  try {
    const names = ["codex/one", "codex/two"];
    names.forEach((name, i) => { catalogState.rows[name] = { key: `run:${i}`, computer: "", name }; });
    native.inspect.mockResolvedValue(reviewOf("/srv/example/a", names));
    applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [keptFolder("a", { sessions: names })], kept: 1 });
    // An error boundary owns the A2 case: a read of the errored resource throws,
    // and that must fail this test by assertion, not only as an unhandled error.
    const caught: unknown[] = [];
    const host = document.createElement("div"); document.body.append(host);
    dispose = render(() => createComponent(ErrorBoundary, { fallback: (error: unknown) => { caught.push(error); return "boundary caught an error"; }, get children() {
      return [createComponent(CleanupReviewButton, {}), Show({ get when() { return cleanupDialog(); }, children: (_current: Accessor<NonNullable<ReturnType<typeof cleanupDialog>>>) =>
        createComponent(Suspense, { get children() { return createComponent(WorktreeCleanup, {}); } }) })];
    } }), host);
    review(host).click(); await flush();
    const probe = detachProbe(host);
    button(host, "Archive these 2 sessions…").click();
    const confirmation = document.querySelector<HTMLElement>("dialog.bc-archive")!;
    await vi.waitFor(() => expect(confirmation.querySelector("[role=alert]")).not.toBeNull());
    const alert = confirmation.querySelector<HTMLElement>("[role=alert]")!;
    // The mockup's danger notice: its tone and the circle-alert icon (zerus-nq26a.5).
    expect([...alert.classList]).toEqual(expect.arrayContaining(["notice", "danger"]));
    expect(alert.querySelector(":scope > [data-icon]")?.getAttribute("data-icon")).toBe("circle-alert");
    // A plain sentence with a next step first; the raw detail only as secondary text.
    expect(alert.querySelector("p:not(.hint)")?.textContent).toBe("Zerus could not load the session actions. Close this dialog and choose Archive again. If it keeps failing, restart Zerus.");
    expect(alert.querySelector("p.hint")?.textContent).toContain("Session actions are unavailable.");
    expect(button(confirmation, "Archive 2 sessions").disabled).toBe(true);
    expect(caught).toEqual([]);
    expect(probe.detached()).toBe(false); probe.stop();
    expect(archiveCalls.calls).toEqual([]);
  } finally { archiveCalls.fail = false; }
});
