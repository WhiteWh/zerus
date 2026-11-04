import { ProjectLabel } from "../components/ProjectLabel";
import ProjectColorDialog from "./ProjectColorDialog";
import ProjectCatalogDialog from "./ProjectCatalogDialog";
import { For, Show, Suspense, lazy, createEffect, createMemo, createResource, createSignal, on, onCleanup, untrack } from "solid-js";
import { Icon } from "../lib/icons";
import { MachineLabel } from "../components/MachineLabel";
import type { EditOutcome, Folder, Project } from "../../types";
import * as organization from "../../store/organization";
import { projectToShow, setProjectToShow } from "../../store/organization";
import { projectsPage } from "../../store/projects";
import { flag, num, setSetting, settingsLoaded } from "../../store/settings";
import { setMenu, type MenuEntry } from "../../store/ui";
import { PROJECT_EDITING } from "./reasons";
import { machineCards } from "../../store/machines";
import { folderButton, folderExists, folderLaunch, folderMenuEntry, newSessionInFolder } from "../sessions/newsession-view";
import { copy } from "../../store/actions";
import { nextSort, showAskedProject, shownProjects, sortFolders, type Sort, type SortColumn } from "./projects-ui";
import { createProjectFolders } from "./project-folders";
const ProjectSwarmDialog = lazy(() => import("./ProjectSwarmDialog"));
const WorktreeOverviewDialog = lazy(() => import("./WorktreeOverviewDialog"));
const ProjectWorktrees = lazy(() => import("./ProjectWorktrees"));
const ProjectCloneDialog = lazy(() => import("./ProjectCloneDialog"));
const ProjectFolderDialog = lazy(() => import("./ProjectFolderDialog"));
import { catalogName, createCatalogEdits } from "./project-catalog";
import { tr } from "../../i18n";

// Projects (ProjectsDialog.cpp), ported from design/webui/src/screens/Projects.tsx:
// the shared project catalog the Sessions list groups by (the organization
// topic, with this device's default project and the project sync status), its
// folders on each computer. The color popup uses the shared catalog writer and
// local presentation settings. Catalog edits share the Sessions list's
// organization writer. Folder/default and Swarm controls have separate work.

const [selectedProject, setSelectedProject] = createSignal<string | undefined>();
const [selectedFolder, setSelectedFolder] = createSignal<string>();
const allSwarm = () => flag("workspace/allSwarmProjects");
const setAllSwarm = (value: boolean) => { void setSetting("workspace/allSwarmProjects", value); };

const disabled = (label: string, icon: string, hint: string): MenuEntry => ({ label, icon, disabled: true, hint });
/** No folder selected: Qt's selectedFolder() is an empty folder. */
const NO_FOLDER: Folder = { id: "", key: "", name: "", path: "", computer: "", machine: "" };

/** InlineProjectName in the mockup: commit on Enter/blur, Escape reverts. */
function ProjectName(props: { project: Project; enabled: boolean; rename(id: string, value: string): Promise<EditOutcome | undefined> | undefined }) {
  const [value, setValue] = createSignal(props.project.name);
  let editing: { id: string; name: string } | undefined;
  createEffect(() => { const p = props.project; if (!editing || p.id !== editing.id) { editing = undefined; setValue(p.name); } });
  const commit = () => {
    const started = editing;
    editing = undefined;
    if (!started) return;
    const name = catalogName(value());
    if (!name || started.id !== props.project.id) { setValue(props.project.name); return; }
    setValue(name);
    void props.rename(started.id, name)?.then((result) => { if (!result || result.outcome === "refused") setValue(props.project.name); });
  };
  return <input id="proj-name" class="field proj-name" maxLength={80} value={value()} readOnly={!props.enabled}
    onFocus={() => { editing = { id: props.project.id, name: props.project.name }; }}
    onInput={(event) => setValue(event.currentTarget.value)} onBlur={commit}
    onKeyDown={(event) => {
      if (event.key === "Enter") { event.preventDefault(); commit(); event.currentTarget.blur(); }
      else if (event.key === "Escape") { event.preventDefault(); editing = undefined; setValue(props.project.name); event.currentTarget.blur(); }
    }} />;
}

export function ProjectsScreen() {
  const view = projectsPage;
  const [colorProject, setColorProject] = createSignal<{ project: Project; origin: HTMLElement }>();
  const [defaultBusy, setDefaultBusy] = createSignal(false);
  const chooseDefault = async (id: string) => { setDefaultBusy(true); try { await organization.setDefaultProject(id); } finally { if (!left) setDefaultBusy(false); } };
  const [sort, setSort] = createSignal<Sort>({ column: "name", ascending: true });
  const projects = createMemo(() => shownProjects(view(), allSwarm()));
  const project = () => projects().find((p) => p.id === selectedProject()) ?? projects().find((p) => p.accessible);
  const [dragSplit, setSplit] = createSignal<number>();
  const split = () => Math.max(160, Math.min(Math.max(160, window.innerHeight - 360), dragSplit() ?? num("workspace/projectFolderSplit")));
  let stopSplit: (() => void) | undefined;
  const startSplit = (event: PointerEvent) => {
    stopSplit?.(); const startY = event.clientY, start = split();
    const move = (next: PointerEvent) => setSplit(Math.max(160, Math.min(window.innerHeight - 360, start + next.clientY - startY)));
    const end = () => {removeEventListener("pointermove", move); removeEventListener("pointerup", end); removeEventListener("pointercancel", end); stopSplit = undefined; void setSetting("workspace/projectFolderSplit", split()); setSplit(undefined);};
    stopSplit = end; addEventListener("pointermove", move); addEventListener("pointerup", end); addEventListener("pointercancel", end);
  };
  onCleanup(() => stopSplit?.());
  const [swarmOrigin, setSwarmOrigin] = createSignal<{ element: HTMLElement; connect: boolean; preferred?: string }>();
  const [overviewOrigin, setOverviewOrigin] = createSignal<HTMLElement>();
  const [cloneOrigin, setCloneOrigin] = createSignal<HTMLElement>();
  const folderEdits = createProjectFolders(() => view().projects, setSelectedFolder);
  const selected = () => project()?.folders.find((f) => f.key === selectedFolder()) ?? project()?.folders[0];
  const folders = createMemo(() => sortFolders(project()?.folders ?? [], sort()));
  let left = false;
  onCleanup(() => { left = true; setMenu(undefined); });
  const choose = (p: Project) => { setSelectedProject(p.id); setSelectedFolder(undefined); };
  const edits = createCatalogEdits({ ...organization, projects: () => view().projects,
    select: (id) => { setSelectedProject(id); setSelectedFolder(undefined); } });
  // Moving a keyed DOM node can blur it in WebKit. Preserve the keyboard
  // origin through our reorder only; another user input cancels the handoff.
  let moveFocus: { element: HTMLElement; order: string } | undefined;
  const order = createMemo(() => projects().map((p) => p.id).join("\n"));
  const clearMoveFocus = () => { moveFocus = undefined; };
  addEventListener("pointerdown", clearMoveFocus, true);
  addEventListener("keydown", clearMoveFocus, true);
  onCleanup(() => { removeEventListener("pointerdown", clearMoveFocus, true); removeEventListener("keydown", clearMoveFocus, true); });
  createEffect(on(order, (next) => {
    const saved = moveFocus;
    if (!saved || next === saved.order) return;
    queueMicrotask(() => {
      if (moveFocus !== saved) return;
      moveFocus = undefined;
      if (!left && saved.element.isConnected && (document.activeElement === document.body || document.activeElement === saved.element)) saved.element.focus();
    });
  }, { defer: true }));
  const move = (id: string, by: -1 | 1) => {
    const element = document.activeElement, before = order();
    const pending = edits.move(id, by);
    if (pending && element instanceof HTMLElement && element.dataset.id === id) {
      const saved = moveFocus = { element, order: before };
      void pending.then((result) => { if (result?.outcome === "refused" && moveFocus === saved) moveFocus = undefined; });
    }
  };
  const editHint = () => organization.grouping().reason ?? undefined;
  const [dragging, setDragging] = createSignal<string>();
  const [dropBefore, setDropBefore] = createSignal<string>();
  const endDrag = () => { setDragging(undefined); setDropBefore(undefined); };
  const drop = (event: DragEvent, before?: string) => {
    if (!dragging()) return;
    event.preventDefault(); event.stopPropagation();
    void edits.drop(dragging()!, before); endDrag();
  };
  // ProjectsDialog.cpp:520-522: the Folders button's New session… follows the selected folder.
  const launchFolder = () => selected() ?? NO_FOLDER;
  // Read again only for another folder or once this computer's identity arrives; the last answer shows meanwhile.
  const launchPlace = () => `${launchFolder().machine}\n${launchFolder().path}\n${machineCards[""]?.host ?? ""}`;
  const [launchExists] = createResource(launchPlace, () => folderExists(untrack(launchFolder)));
  const launchButton = () => folderButton(folderLaunch(launchFolder(), launchExists.latest), launchFolder().machine);
  const launchSelected = () => { const p = project(), f = selected(); if (p && f) void newSessionInFolder(p.id, f); };
  showAskedProject(projectToShow, () => setProjectToShow(undefined), view, choose);

  let menuAsked = 0;
  const folderMenu = async (e: MouseEvent, index: number) => {
    e.preventDefault();
    setSelectedFolder(project()?.folders[index]?.key);
    const { clientX: x, clientY: y } = e;
    const origin = e.currentTarget as HTMLElement;
    const current = project();
    const others = projects().filter((p) => p.accessible && p.id !== current?.id);
    const folder = current?.folders[index] ?? NO_FOLDER;
    const asked = ++menuAsked;
    // ProjectsDialog.cpp:556-558, checked as the menu opens: this computer's folder must exist.
    const launch = folderMenuEntry(folderLaunch(folder, await folderExists(folder)));
    // Opened for that row only: not once Projects was left, another row was chosen or a newer menu was asked.
    const now = project()?.folders.find((f) => f.key === folder.key) ?? NO_FOLDER;
    if (left || asked !== menuAsked || project()?.id !== current?.id || selected()?.key !== folder.key
      || now.key !== folder.key || now.machine !== folder.machine || now.path !== folder.path) return;
    setMenu({ x, y, items: [
      { label: tr("New session…"), icon: "plus", ...launch, action: () => { if (current) void newSessionInFolder(current.id, folder); } },
      { label: tr("Open folder"), icon: "folder-open", disabled: folder.machine !== machineCards[""]?.host || launch.disabled, action: () => { if (current) void folderEdits.openFolder(current.id, folder); } },
      // ProjectsDialog.cpp:565-566: always enabled, copies the path the row shows.
      { label: tr("Copy folder path"), icon: "copy", hint: folder.path, action: () => { void copy(folder.path); } },
      { separator: true },
      { heading: tr("Copy to") },
      ...others.map((p) => ({ label: p.name, icon: "folder", disabled: !current || !folderEdits.enabled(current.id, folder) || !folderEdits.enabled(p.id), action: () => { if (current) void folderEdits.transfer(current.id, folder, p.id, true); } })),
      { heading: tr("Move to") },
      ...others.map((p) => ({ label: p.name, icon: "arrow-right", disabled: !current || !folderEdits.enabled(current.id, folder) || !folderEdits.enabled(p.id), action: () => { if (current) void folderEdits.transfer(current.id, folder, p.id, false); } })),
      { separator: true },
      { label: tr("Edit folder…"), icon: "pencil", disabled: !current || !folderEdits.enabled(current.id, folder), action: () => { if (current) void folderEdits.open(current.id, folder, origin); } },
      { label: tr("Remove folder from project"), icon: "minus", disabled: !current || !folderEdits.enabled(current.id, folder), action: () => { if (current) void folderEdits.remove(current.id, folder); } },
      { label: tr("Add folder…"), icon: "plus", disabled: !current || !folderEdits.enabled(current.id), action: () => { if (current) void folderEdits.open(current.id, undefined, origin); } },
    ] });
  };

  const listMenu = (e: MouseEvent, id?: string) => {
    e.preventDefault();
    const origin = e.currentTarget as HTMLElement;
    const p = projects().find((x) => x.id === id);
    if (p) choose(p);
    setMenu({ x: e.clientX, y: e.clientY, items: [
      { label: tr("New project…"), icon: "plus", disabled: !edits.enabled(), hint: editHint(), action: () => edits.openNew(origin) },
      ...(p ? [{ separator: true } as const,
        { label: tr("Move project up"), icon: "arrow-up", kbd: "Alt+↑", disabled: !edits.canEdit(p.id) || projects().findIndex((x) => x.id === p.id) <= 0, hint: editHint(), action: () => move(p.id, -1) },
        { label: tr("Move project down"), icon: "arrow-down", kbd: "Alt+↓", disabled: !edits.canEdit(p.id) || projects().findIndex((x) => x.id === p.id) >= projects().length - 1, hint: editHint(), action: () => move(p.id, 1) },
      ] : []),
    ] });
  };

  const onListKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
    e.preventDefault();
    if (e.altKey) { const id = project()?.id; if (id) move(id, e.key === "ArrowUp" ? -1 : 1); return; }
    const list = projects().filter((p) => p.accessible);
    const i = list.findIndex((p) => p.id === project()?.id);
    const next = list[Math.min(list.length - 1, Math.max(0, i + (e.key === "ArrowUp" ? -1 : 1)))];
    if (!next) return;
    choose(next);
    (e.currentTarget as HTMLElement).querySelector<HTMLElement>(`[data-id="${CSS.escape(next.id)}"]`)?.focus();
  };

  const header = (column: SortColumn, label: string) => (
    <th aria-sort={sort().column === column ? (sort().ascending ? "ascending" : "descending") : "none"}>
      <button class="su-th" title={tr("Click a column heading to sort folders.")} onClick={() => setSort((s) => nextSort(s, column))}>
        {label}<Show when={sort().column === column}><Icon name={sort().ascending ? "arrow-up" : "arrow-down"} size={11} /></Show>
      </button>
    </th>
  );

  return (
    <div class="su-page projects">
      <div class="proj-content">
        <header class="su-head">
          <h1>{tr("Projects")}</h1>
          <span class="grow" />
          <button class="btn" disabled={!organization.grouping().ready} onClick={(e) => setOverviewOrigin(e.currentTarget)}><Icon name="folder-git-2" size={14} />{tr("Worktrees…")}</button>
          <button class="btn" disabled={!view().loaded} onClick={(e) => setSwarmOrigin({ element: e.currentTarget, connect: false })}><Icon name="network" size={14} />{tr("Swarm…")}</button>
        </header>
        <p class="muted proj-hint">{tr("Organize sessions and folders across your computers. A folder can be used by several projects.")}</p>

        <Show when={view().notice}>{(notice) => (
          <div class="notice proj-guide">
            <Icon name="link" size={14} />
            <span class="grow">{notice()}</span>
            <button class="btn small" disabled={!view().loaded} onClick={(e) => setSwarmOrigin({ element: e.currentTarget, connect: true, preferred: view().unconnected[0] })}>{tr("Connect project catalogs…")}</button>
          </div>
        )}</Show>

        <div class="row proj-swarm">
          <label class="check" title={tr("Include projects on computers without a configured connection here.")}>
            <input type="checkbox" checked={allSwarm()} onChange={(e) => setAllSwarm(e.currentTarget.checked)} />{tr("All swarm projects")}
          </label>
          <span class="muted" classList={{ danger: view().status_error }}>{view().status}</span>
        </div>

        <div class="proj-split">
          <aside class="proj-left">
            <ul class="proj-list" role="listbox" aria-label={tr("Projects")} tabindex="-1" onKeyDown={onListKey}
              onDragOver={(event) => { if (dragging()) { event.preventDefault(); setDropBefore(undefined); } }} onDrop={(event) => drop(event)}
              onContextMenu={(e) => { if (e.target === e.currentTarget) listMenu(e); }}>
              <For each={projects().map((p) => p.id)}>{(id) => {
                const p = createMemo<Project>((last) => projects().find((p) => p.id === id) ?? last, untrack(() => projects().find((p) => p.id === id)!));
                return (
                <li role="option" aria-selected={p().id === project()?.id} classList={{ "drop-before": dropBefore() === p().id }}
                  onDragOver={(event) => { if (dragging() && edits.canEdit(p().id)) { event.preventDefault(); event.stopPropagation(); setDropBefore(p().id); } }} onDrop={(event) => drop(event, p().id)}>
                  <Show when={p().accessible} fallback={<div class="proj-item remote" title={p().name}><ProjectLabel project={p()} /></div>}>
                    <button class="proj-item" classList={{ on: p().id === project()?.id, dragging: dragging() === p().id }} data-id={p().id} tabindex={p().id === project()?.id ? 0 : -1}
                      draggable={edits.canEdit(p().id)} onDragStart={(event) => { if (!edits.canEdit(p().id)) { event.preventDefault(); return; } setDragging(p().id); event.dataTransfer?.setData("application/x-zerus-project", p().id); if (event.dataTransfer) event.dataTransfer.effectAllowed = "move"; }} onDragEnd={endDrag}
                      title={`${p().is_default ? tr("%1, default project for sessions started outside Zerus", p().name) : p().name}\n${tr("Drag to reorder, or use Alt+Up / Alt+Down.")}`}
                      onClick={() => choose(p())} onContextMenu={(e) => listMenu(e, p().id)}>
                      <ProjectLabel project={p()} />
                    </button>
                  </Show>
                </li>
              ); }}</For>
            </ul>
            <button class="btn" disabled={!edits.enabled()} title={editHint()} onClick={(event) => edits.openNew(event.currentTarget)}><Icon name="plus" size={14} />{tr("New project…")}</button>
          </aside>

          <Show when={project()}>{(p) => (
            <section class="proj-right" aria-label={tr("Project")}>
              <div class="form-row">
                <label for="proj-name">{tr("Name")}</label>
                <div class="row">
                  <ProjectName project={p()} enabled={edits.canEdit(p().id)} rename={edits.rename} />
                  <span class="dim proj-id">{tr("ID")}</span>
                  <span class="mono proj-id-val" title={tr("Project ID")}>{p().id}</span>
                </div>
              </div>
              <div class="form-row">
                <label>{tr("Color")}</label>
                <div class="row">
                  <ProjectLabel project={p()} class="proj-preview" />
                  <button class="btn" disabled={!p().accessible || !settingsLoaded()} onClick={(event) => setColorProject({ project: { ...p() }, origin: event.currentTarget })}><Icon name="palette" size={14} />{tr("Edit color…")}</button>
                </div>
              </div>
              <label class="check proj-default" title={editHint()}>
                <input type="checkbox" checked={p().is_default} disabled={defaultBusy() || !edits.canEdit(p().id)}
                  onChange={(event) => { const checked = event.currentTarget.checked; event.currentTarget.checked = p().is_default; if (checked && !p().is_default) void chooseDefault(p().id); }} />{tr("Default for sessions started outside Zerus")}
              </label>

              <div class="proj-folders" style={{height: `${split()}px`}}>
                <div class="row proj-fhead">
                  <h2 class="section-title">{tr("Folders")}</h2>
                  <span class="grow" />
                  <button class="btn small" disabled={!organization.grouping().ready} onClick={(e) => setCloneOrigin(e.currentTarget)}><Icon name="download" size={12} />{tr("Clone repository…")}</button>
                  <button class="btn small" disabled={launchButton().disabled} title={launchButton().tip} onClick={launchSelected}><Icon name="plus" size={12} />{tr("New session…")}</button>
                </div>
                <div class="su-tablewrap">
                  <table class="su-table proj-table">
                    <thead><tr>{header("name", tr("Name"))}{header("machine", tr("Computer"))}{header("path", tr("Folder"))}</tr></thead>
                    <tbody>
                      <For each={folders()}>{(row) => (
                        <tr classList={{ on: selected()?.key === row.folder.key }} tabindex="0" aria-selected={selected()?.key === row.folder.key}
                          onClick={() => setSelectedFolder(row.folder.key)} onContextMenu={(e) => folderMenu(e, row.index)}>
                          <td class="ell" title={row.folder.name}>{row.folder.name}</td>
                          <td><MachineLabel alias={row.folder.computer} /></td>
                          <td class="mono ell" title={row.folder.path}>{row.folder.path}</td>
                        </tr>
                      )}</For>
                    </tbody>
                  </table>
                </div>
                <div class="row proj-factions">
                  <button class="btn small" disabled={!folderEdits.enabled(p().id)} onClick={(e) => void folderEdits.open(p().id, undefined, e.currentTarget)}>{tr("Add folder…")}</button>
                  <button class="btn small" disabled={!folderEdits.enabled(p().id, selected()) || !selected()} onClick={(e) => { const f = selected(); if (f) void folderEdits.open(p().id, { ...f }, e.currentTarget); }}>{tr("Edit folder…")}</button>
                  <button class="btn small" disabled={!folderEdits.enabled(p().id, selected()) || !selected()} title={tr("Remove this folder from the project. Files stay in place.")} onClick={() => { const f = selected(); if (f) void folderEdits.remove(p().id, { ...f }); }}>{tr("Remove folder")}</button>
                  <span class="grow" />
                  <button class="btn small" disabled={!selected() || selected()?.machine !== machineCards[""]?.host || launchButton().disabled} onClick={() => { const f = selected(); if (f) void folderEdits.openFolder(p().id, { ...f }); }}><Icon name="folder-open" size={12} />{tr("Open folder")}</button>
                </div>
              </div>
              <div class="su-hsplit" role="separator" aria-orientation="horizontal" title={tr("Drag to resize folders and worktrees.")} onPointerDown={startSplit} />
              <Suspense><ProjectWorktrees folder={selected()} project={p().id} /></Suspense>
              <div class="row proj-foot">
                <button class="btn danger" disabled={!edits.canEdit(p().id) || view().projects.filter((p) => p.accessible).length < 2} title={editHint()}
                  onClick={(event) => edits.openDelete(p().id, event.currentTarget)}><Icon name="trash" size={14} />{tr("Delete project…")}</button>
              </div>
            </section>
          )}</Show>
        </div>
      </div>
      <Suspense><Show when={swarmOrigin()} keyed>{(origin) => <ProjectSwarmDialog projects={view().projects} connect={origin.connect} preferred={origin.preferred} origin={origin.element} close={() => setSwarmOrigin(undefined)} />}</Show></Suspense>
      <Suspense><Show when={overviewOrigin()} keyed>{(origin) => <WorktreeOverviewDialog projects={view().projects} all={allSwarm()} origin={origin} close={() => setOverviewOrigin(undefined)} />}</Show></Suspense>
      <Suspense><Show when={cloneOrigin()} keyed>{(origin) => <ProjectCloneDialog project={project()?.id ?? "ungrouped"} preferred={selected()?.machine} projects={view().projects} origin={origin} close={() => setCloneOrigin(undefined)} placed={(project, folder) => {setSelectedProject(project); setSelectedFolder(folder);}} />}</Show></Suspense>
      <Suspense><Show when={folderEdits.prompt()} keyed>{(prompt) => <ProjectFolderDialog prompt={prompt} busy={folderEdits.busy()} folders={view().projects.find((p) => p.id === prompt.project)?.folders ?? []} close={folderEdits.close} save={(place, path, name) => { void folderEdits.save(place, path, name); }} />}</Show></Suspense>
      <Show when={colorProject()}>{(p) => <ProjectColorDialog project={p().project} origin={p().origin} close={() => setColorProject(undefined)} />}</Show>
      <Show when={edits.prompt()} keyed>{(prompt) => <ProjectCatalogDialog prompt={prompt} enabled={edits.enabled()}
        close={edits.close} create={(name) => { void edits.submitNew(name); }} remove={(destination) => { void edits.confirmDelete(destination); }} />}</Show>
    </div>
  );
}
