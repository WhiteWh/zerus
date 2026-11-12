// @vitest-environment happy-dom
// zerus-nq26a.3: a selected task beyond the loaded rows is read on its own. That
// read must not suspend the task view's Suspense boundary (SessionDetail mounts
// TaskView inside one), or the view blanks instead of showing its own status.
import { afterEach, expect, it, vi } from "vitest";
import { createComponent, createRenderEffect, Suspense } from "solid-js";
import { render } from "solid-js/web";

const state = vi.hoisted(() => ({ read: vi.fn(), adopt: vi.fn(), rows: [] as { uuid: string }[] }));
vi.mock("../../store/taskContext", () => ({ taskProject: () => ({ id: "p1" }), projectTasks: () => state.rows, findTaskSession: () => () => undefined }));
vi.mock("../../store/tasks", () => ({ tasks: { loaded: () => true, tasks: () => [], adopt: state.adopt } }));
vi.mock("../../store/taskNavigation", async () => {
  const { createSignal } = await import("solid-js");
  const [selectedTask, setSelectedTask] = createSignal<{ project: string; uuid: string }>();
  return { selectedTask, setSelectedTask };
});
vi.mock("../../store/ui", () => ({ setSelectedKey: vi.fn(), setDetailTab: vi.fn() }));
vi.mock("./task-view-read", () => ({ readSelectedTask: state.read }));
vi.mock("./tasks-ui", () => ({ executorRef: () => undefined, projectTask: (task: unknown) => task }));
vi.mock("./TaskInspector", () => ({ TaskInspector: (props: { task: { uuid: string } }) => {
  const shown = document.createElement("div"); shown.className = "ti";
  createRenderEffect(() => { shown.textContent = props.task.uuid; });
  return shown;
} }));
vi.mock("../../i18n", () => ({ tr: (s: string) => s }));
import TaskView from "./TaskView";
import { setSelectedTask } from "../../store/taskNavigation";

const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
function deferred<T>() { let resolve!: (value: T) => void, reject!: (error: unknown) => void; const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; }); return { promise, resolve, reject }; }
let dispose: (() => void) | undefined;
afterEach(() => { dispose?.(); dispose = undefined; document.body.replaceChildren(); vi.clearAllMocks(); });

function mount() {
  const host = document.createElement("div"); document.body.append(host);
  dispose = render(() => createComponent(Suspense, { get children() { return createComponent(TaskView, {}); } }), host);
  return host;
}
// The reviewer's probe: records any moment the view leaves the DOM, even if it comes back.
function detachProbe(host: HTMLElement, selector: string) {
  const watched = host.querySelector(selector)!; expect(watched).not.toBeNull();
  // Records-based: a removal that is undone a microtask later still counts.
  let detached = false;
  const scan = (records: MutationRecord[]) => { for (const record of records) for (const node of record.removedNodes) if (node === watched || node.contains(watched)) detached = true; };
  const observer = new MutationObserver(scan);
  observer.observe(host, { childList: true, subtree: true });
  return { detached: () => { scan(observer.takeRecords()); return detached || !watched.isConnected; }, stop: () => observer.disconnect() };
}

it("keeps the task view and its Loading task… status while a task beyond the loaded rows is read", async () => {
  state.rows = [{ uuid: "a" }];
  const pending = deferred<{ uuid: string }>(); state.read.mockReturnValue(pending.promise);
  setSelectedTask({ project: "p1", uuid: "a" });
  const host = mount(); await flush();
  expect(host.querySelector(".ti")?.textContent).toBe("a");
  const probe = detachProbe(host, ".tv");
  setSelectedTask({ project: "p1", uuid: "b" }); await flush();
  expect(probe.detached()).toBe(false);
  expect(host.querySelector(".tv [role=status]")?.textContent).toBe("Loading task…");
  expect(state.read).toHaveBeenCalledWith("p1\nb");
  pending.resolve({ uuid: "b" }); await flush();
  expect(host.querySelector(".ti")?.textContent).toBe("b");
  expect(probe.detached()).toBe(false); probe.stop();
});

it("shows the read failure in place of the task", async () => {
  state.rows = [{ uuid: "a" }];
  const pending = deferred<{ uuid: string }>(); state.read.mockReturnValue(pending.promise);
  setSelectedTask({ project: "p1", uuid: "a" });
  const host = mount(); await flush();
  const probe = detachProbe(host, ".tv");
  setSelectedTask({ project: "p1", uuid: "c" }); await flush();
  pending.reject(new Error("no task c in this project")); await flush();
  expect(host.querySelector(".tv [role=status]")?.textContent).toBe("This task could not be read.");
  expect(probe.detached()).toBe(false); probe.stop();
});
