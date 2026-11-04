import { createSignal, onCleanup } from "solid-js";
import type { EditOutcome, GroupingOut, Project } from "../../types";
import { deleteText } from "../sessions/organization-ui";

export type ProjectPrompt =
  | { kind: "new"; origin: HTMLElement }
  | { kind: "delete"; origin: HTMLElement; project: string; text: string; options: Project[]; selected: string };

export interface CatalogEdits {
  projects(): readonly Project[];
  grouping(): GroupingOut;
  createProject(name: string): Promise<EditOutcome>;
  renameProject(project: string, name: string): Promise<EditOutcome>;
  deleteProject(project: string, destination: string): Promise<EditOutcome>;
  moveProject(project: string, before: string | undefined): Promise<EditOutcome>;
  select(project: string): void;
}

/** The mockup's inline name: one line, 1–80 characters; empty editing reverts. */
export const catalogName = (value: string): string | undefined => {
  const name = value.trim();
  return name && Array.from(name).length <= 80 && !/[\r\n]/u.test(name) ? name : undefined;
};

/** Projects uses the existing organization writer, never a second apply lane. */
export function createCatalogEdits(api: CatalogEdits) {
  const [prompt, setPrompt] = createSignal<ProjectPrompt>();
  const [busy, setBusy] = createSignal(false);
  let left = false;
  onCleanup(() => { left = true; });
  const listed = (id: string) => api.projects().find((p) => p.id === id && p.accessible);
  const enabled = () => !busy() && api.grouping().ready;
  const canEdit = (id: string) => enabled() && !!listed(id);
  const close = () => setPrompt(undefined);
  const run = async (action: () => Promise<EditOutcome>) => {
    if (!enabled()) return;
    setBusy(true);
    try { return await action(); }
    finally { if (!left) setBusy(false); }
  };
  const openNew = (origin: HTMLElement) => { if (enabled()) setPrompt({ kind: "new", origin }); };
  const openDelete = (id: string, origin: HTMLElement) => {
    const p = listed(id), options = api.projects().filter((p) => p.accessible && p.id !== id);
    if (!canEdit(id) || !p || !options.length) return;
    const selected = options.find((p) => p.id === api.grouping().default_project)?.id ?? options[0].id;
    setPrompt({ kind: "delete", origin, project: id, text: deleteText(p.name), options: [...options], selected });
  };
  const submitNew = async (value: string) => {
    const name = catalogName(value);
    if (prompt()?.kind !== "new" || !name || !enabled()) return;
    close();
    const result = await run(() => api.createProject(name));
    // The ID belongs to this exact create, including a kept apply. Equal names
    // from concurrent writers never select somebody else's project.
    if (!left && result && result.outcome !== "refused" && result.project) api.select(result.project);
  };
  const confirmDelete = async (destination: string) => {
    const p = prompt();
    if (p?.kind !== "delete" || !canEdit(p.project) || !listed(destination)
      || destination === p.project || !p.options.some((option) => option.id === destination)) return;
    close();
    const result = await run(() => api.deleteProject(p.project, destination));
    if (!left && result && result.outcome !== "refused") api.select(destination);
  };
  const rename = (id: string, value: string) => {
    const name = catalogName(value);
    if (!name || !canEdit(id) || listed(id)?.name === name) return;
    return run(() => api.renameProject(id, name));
  };
  const move = (id: string, by: -1 | 1) => {
    // Resolve neighbours at activation, so a menu opened before another
    // writer's reorder never applies a stale position.
    const projects = api.projects().filter((p) => p.accessible), at = projects.findIndex((p) => p.id === id);
    if (!canEdit(id) || at < 0 || at + by < 0 || at + by >= projects.length) return;
    return run(() => api.moveProject(id, by < 0 ? projects[at - 1].id : projects[at + 2]?.id));
  };
  const drop = (id: string, before: string | undefined) => {
    if (!canEdit(id) || (before !== undefined && (!listed(before) || before === id))) return;
    return run(() => api.moveProject(id, before));
  };
  return { prompt, busy, enabled, canEdit, openNew, openDelete, close, submitNew, confirmDelete, rename, move, drop };
}
