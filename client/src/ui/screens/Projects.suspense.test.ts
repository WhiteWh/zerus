// @vitest-environment happy-dom
// zerus-nq26a.3: App.tsx mounts the Projects page inside a Suspense boundary.
// The New session button's folder check must never suspend it: a project that
// appears while the first check is still pending used to detach the page.
import { afterEach, expect, it, vi } from "vitest";
import { createComponent, Suspense } from "solid-js";
import { render } from "solid-js/web";
import type { Project, ProjectsView } from "../../types";

const state = vi.hoisted(() => ({ exists: vi.fn() }));
vi.mock("../../store/projects", async () => {
  const { createSignal } = await import("solid-js");
  const [projectsPage, setProjectsPage] = createSignal<ProjectsView>({ loaded: true, status: "", status_error: false, conflicts: 0, unconnected: [], notice: null, projects: [], swarm_id: "" });
  return { projectsPage, setProjectsPage };
});
vi.mock("../../store/organization", () => ({ grouping: () => ({ ready: true, headers: [] }), projectToShow: () => undefined, setProjectToShow: vi.fn(), setDefaultProject: vi.fn() }));
vi.mock("../../store/settings", () => ({ flag: () => false, num: () => 240, setSetting: vi.fn(), settingsLoaded: () => true }));
vi.mock("../../store/ui", () => ({ setMenu: vi.fn() }));
vi.mock("../../store/machines", () => ({ machineCards: {} }));
vi.mock("../../store/actions", () => ({ copy: vi.fn() }));
vi.mock("../../store/notices", () => ({ bus: { notify: vi.fn() } }));
vi.mock("../../store/legacyProjects", () => ({ legacyProjectStatus: (s: string) => s }));
vi.mock("../../bridge", () => ({}));
vi.mock("../sessions/newsession-view", () => ({
  folderExists: state.exists,
  folderLaunch: (folder: { path: string }, exists?: boolean) => ({ problem: exists === false ? "This folder is unavailable on this computer." : "", shown: folder.path, local: true }),
  folderButton: (launch: { problem: string; shown: string }) => ({ disabled: !!launch.problem, tip: launch.problem || `New session in ${launch.shown}` }),
  folderMenuEntry: vi.fn(), newSessionInFolder: vi.fn(),
}));
vi.mock("./ProjectColorDialog", () => ({ default: () => null }));
vi.mock("./ProjectCatalogDialog", () => ({ default: () => null }));
vi.mock("../components/ProjectLabel", () => ({ ProjectLabel: () => null }));
vi.mock("../components/MachineLabel", () => ({ MachineLabel: () => null }));
vi.mock("../lib/TextField", () => ({ TextInput: () => document.createElement("input") }));
vi.mock("../lib/icons", () => ({ Icon: () => null }));
vi.mock("../../i18n", () => ({ tr: (s: string, ...args: unknown[]) => s.replace(/%(\d+)/g, (_, n) => String(args[Number(n) - 1])), trNoop: (s: string) => s }));
import { ProjectsScreen } from "./Projects";
import { setProjectsPage } from "../../store/projects";

const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((ok) => { resolve = ok; }); return { promise, resolve }; }
let dispose: (() => void) | undefined;
afterEach(() => { dispose?.(); dispose = undefined; document.body.replaceChildren(); vi.clearAllMocks(); });

const project: Project = { id: "p1", name: "example", color: null, accessible: true, is_default: true, palette: 1,
  folders: [{ id: "f1", key: "f1", name: "example", path: "/srv/example", computer: "", machine: "this computer" }] };
const view = (projects: Project[]): ProjectsView => ({ loaded: true, status: "", status_error: false, conflicts: 0, unconnected: [], notice: null, projects, swarm_id: "" });

it("keeps the Projects page attached when a project appears before the first folder check answers", async () => {
  const first = deferred<boolean | undefined>();
  state.exists.mockReturnValueOnce(first.promise).mockResolvedValue(true);
  setProjectsPage(view([]));
  const host = document.createElement("div"); document.body.append(host);
  dispose = render(() => createComponent(Suspense, { get children() { return createComponent(ProjectsScreen, {}); } }), host);
  await flush();
  const page = host.querySelector(".su-page.projects")!; expect(page).not.toBeNull();
  // Records-based probe: a removal that is undone a microtask later still counts.
  let detached = false;
  const scan = (records: MutationRecord[]) => { for (const record of records) for (const node of record.removedNodes) if (node === page || node.contains(page)) detached = true; };
  const observer = new MutationObserver(scan);
  observer.observe(host, { childList: true, subtree: true });
  // The project arrives while the first check (for no folder yet) is still pending.
  setProjectsPage(view([project])); await flush(); scan(observer.takeRecords());
  expect(detached || !page.isConnected).toBe(false);
  const launch = [...host.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.includes("New session…"));
  expect(launch).toBeTruthy();
  first.resolve(undefined); await flush(); scan(observer.takeRecords()); observer.disconnect();
  expect(detached || !page.isConnected).toBe(false);
  // The selected folder's own check answers and still drives the button.
  expect(state.exists).toHaveBeenLastCalledWith(expect.objectContaining({ path: "/srv/example" }));
  expect(launch!.disabled).toBe(false);
});

it("keeps the last folder answer while another folder is checked, without detaching the page", async () => {
  const two: Project = { ...project, folders: [project.folders[0], { id: "f2", key: "f2", name: "other", path: "/srv/other", computer: "", machine: "this computer" }] };
  const second = deferred<boolean | undefined>();
  state.exists.mockResolvedValueOnce(false).mockReturnValueOnce(second.promise);
  setProjectsPage(view([two]));
  const host = document.createElement("div"); document.body.append(host);
  dispose = render(() => createComponent(Suspense, { get children() { return createComponent(ProjectsScreen, {}); } }), host);
  await flush();
  const launch = () => [...host.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.includes("New session…"))!;
  expect(launch().disabled).toBe(true); // The first folder's answer: missing.
  const page = host.querySelector(".su-page.projects")!;
  let detached = false;
  const scan = (records: MutationRecord[]) => { for (const record of records) for (const node of record.removedNodes) if (node === page || node.contains(page)) detached = true; };
  const observer = new MutationObserver(scan);
  observer.observe(host, { childList: true, subtree: true });
  [...host.querySelectorAll<HTMLTableRowElement>("tbody tr")].find((row) => row.textContent?.includes("other"))!.click(); await flush();
  expect(state.exists).toHaveBeenLastCalledWith(expect.objectContaining({ path: "/srv/other" }));
  // The last answer shows meanwhile; the page stays attached.
  expect(launch().disabled).toBe(true);
  second.resolve(true); await flush();
  expect(launch().disabled).toBe(false);
  scan(observer.takeRecords()); observer.disconnect();
  expect(detached || !page.isConnected).toBe(false);
});
