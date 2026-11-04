import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { enhance } from "./ProjectCatalogDialog";
import { FolderBrowserDialog } from "../components/FolderBrowser";
import { machineCards } from "../../store/machines";
import { bus, toast } from "../../store/notices";
import * as bridge from "../../bridge";
import type { Project } from "../../types";
import type { Place } from "./project-folders";
import "../styles/project-clone.css";
import { tr } from "../../i18n";
export interface CloneView { id: string; url: string; parent: string; name: string; destination: string; status: string; busy: boolean; cloning: boolean; uncertain: boolean; ready: boolean; locked: boolean; can_submit: boolean }
const NEW = "__new__";
/** setup-clone-computer / setup-clone. The native adapter owns the operation. */
export default function ProjectCloneDialog(props: { project: string; preferred?: string; projects: Project[]; origin: HTMLElement; close(): void; placed(project: string, folder: string): void }) {
  const places: Place[] = Object.entries(machineCards).filter(([id, m]) => m.ok && m.host && m.machine_id && (id !== "" || (m.local_sessions !== false && m.platform !== "windows"))).map(([computer, m]) => ({ computer, host: m.host!, machine_id: m.machine_id!, label: computer || `${m.host} (this computer)` }));
  const [computer, setComputer] = createSignal(places.find((x) => (x.computer || x.host) === props.preferred)?.computer ?? places[0]?.computer ?? "");
  const [place, setPlace] = createSignal<Place>();
  const [view, setView] = createSignal<CloneView>();
  const [url, setUrl] = createSignal(""); const [parent, setParent] = createSignal("~"); const [name, setName] = createSignal("");
  const [project, setProject] = createSignal(props.project); const [newName, setNewName] = createSignal("");
  const [busy, setBusy] = createSignal(false); const [error, setError] = createSignal(""); const [browsing, setBrowsing] = createSignal(false);
  let gone = false, nameEdited = false, suggestion = 0, dialog!: HTMLDialogElement;
  const sync = (v: CloneView) => { if (!gone) { setView(v); setUrl(v.url); setParent(v.parent); setName(v.name); } };
  const locked = () => busy() || !!view()?.locked;
  const destination = () => name() ? `${parent().replace(/\/$/u, "")}/${name()}` : "";
  const validProject = () => project() === NEW ? !!newName().trim() : props.projects.some((p) => p.id === project() && p.accessible);
  const valid = () => !!url().trim() && !!parent().trim() && !!name() && name() !== "." && name() !== ".." && !/[\u0000-\u001f\u007f/\\:*?"<>|]/u.test(name()) && validProject();
  const close = async () => {
    if (view()?.cloning && view()?.busy) return;
    try { if (view()) await bridge.projectCloneClose(view()!.id); props.close(); }
    catch (e) {setError(String(e));}
  };
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLElement>("select")?.focus()); });
  onCleanup(() => { gone = true; if (view()) void bridge.projectCloneClose(view()!.id).catch(() => {}); dialog.close(); if (props.origin.isConnected) props.origin.focus(); });
  const open = async () => {
    const selected = places.find((p) => p.computer === computer()); if (!selected || busy()) return;
    setBusy(true); setError("");
    try { const v = await bridge.projectCloneOpen(selected); if (!gone) { setPlace(selected); sync(v); nameEdited = !!v.name; queueMicrotask(() => dialog.querySelector<HTMLElement>("#cl-url")?.focus()); } }
    catch (e) {setError(String(e));} finally {if (!gone) setBusy(false);}
  };
  const changeUrl = async (value: string) => {
    setUrl(value); const serial = ++suggestion;
    if (!nameEdited) {const fields = await bridge.repositoryFields(value, "", "posix", false).catch(() => null); if (!gone && serial === suggestion && !nameEdited && fields) setName(fields.suggested);}
  };
  const step = async (action: "begin" | "check" | "retire") => {
    const v = view(); if (!v || busy() || (action === "begin" && !valid())) return;
    setBusy(true); setError("");
    let polling = false, watching = true;
    const poll = setInterval(() => {if (!polling) {polling = true; void bridge.projectCloneStatus(v.id).then((next) => {if (!gone && watching) setView(next);}).catch(() => {}).finally(() => {polling = false;});}}, 150);
    try {sync(await bridge.projectCloneStep(v.id, action, {url: url().trim(), parent: parent().trim(), name: name()})); if (action === "retire") nameEdited = true;}
    catch (e) {setError(String(e)); const next = await bridge.projectCloneStatus(v.id).catch(() => null); if (next) sync(next);}
    finally {watching = false; clearInterval(poll); if (!gone) setBusy(false);}
  };
  const use = async () => {
    const v = view(); if (!v?.ready || !validProject() || busy()) return;
    setBusy(true); setError("");
    try {
      const out = await bridge.projectCloneUse(v.id, project() === NEW ? null : project(), newName().trim());
      if (out.outcome.outcome === "refused") {setError(out.outcome.detail); return;}
      if (out.outcome.outcome === "kept") bus.notify("warn", out.outcome.detail);
      else toast(tr("Checkout added"), `${v.destination} on ${place()!.host}`);
      props.placed(out.project, out.folder); props.close();
    } catch (e) {setError(String(e));} finally {if (!gone) setBusy(false);}
  };
  const browse = async () => {if (place()!.computer) setBrowsing(true); else {const p = await bridge.pickLocalFolder(parent()).catch(() => null); if (p) setParent(p);}};
  return <>
    <dialog ref={dialog} class="dlg su-dlg" style={{width: place() ? "640px" : "460px"}} aria-labelledby="project-clone-title" onCancel={(e) => {e.preventDefault(); void close();}}>
      <div class="dlg-head"><Icon name="download" size={18} class="accent" /><h2 id="project-clone-title">{tr("Clone repository")}</h2><span class="grow" /><button type="button" class="icon-btn" aria-label={tr("Close")} disabled={view()?.cloning && view()?.busy} onClick={() => void close()}><Icon name="x" size={16} /></button></div>
      <Show when={place()} fallback={<>
        <div class="dlg-body"><div class="form-row"><label for="clone-on">{tr("Clone on computer")}</label><select ref={enhance} id="clone-on" class="field su-rsel" value={computer()} disabled={busy()} onChange={(e) => setComputer(e.currentTarget.value)}><For each={places}>{(p) => <option value={p.computer}>{p.label}</option>}</For></select></div><Show when={error()}><p class="clone-state danger" role="status">{error()}</p></Show></div>
        <div class="dlg-foot"><span class="grow" /><button class="btn" onClick={() => void close()}>{tr("Cancel")}</button><button class="btn primary" disabled={!places.length || busy()} onClick={() => void open()}>{tr("Continue")}</button></div>
      </>}>
        <div class="dlg-body">
          <p class="muted">{tr("Computer: %1", place()?.host ?? "")}</p>
          <div class="form-row"><label for="cl-url">{tr("Repository")}</label><input id="cl-url" class="field mono" maxLength={4096} placeholder="git@example.org:team/project.git" disabled={locked()} value={url()} onInput={(e) => void changeUrl(e.currentTarget.value)} /></div>
          <div class="form-row"><label for="cl-parent">{tr("Parent folder")}</label><div class="row"><input id="cl-parent" class="field mono grow" maxLength={32768} disabled={locked()} value={parent()} onInput={(e) => setParent(e.currentTarget.value)} /><button class="btn" disabled={locked()} onClick={() => void browse()}>{tr("Browse…")}</button></div></div>
          <div class="form-row"><label for="cl-name">{tr("Folder name")}</label><input id="cl-name" class="field mono" maxLength={80} disabled={locked()} value={name()} onInput={(e) => {nameEdited = true; setName(e.currentTarget.value);}} /></div>
          <div class="form-row"><label>{tr("Destination")}</label><span class="mono su-select ell" title={destination()}>{destination() || "—"}</span></div>
          <div class="form-row"><label for="cl-proj">{tr("Project")}</label><select ref={enhance} id="cl-proj" class="field su-rsel" value={project()} disabled={locked()} onChange={(e) => setProject(e.currentTarget.value)}><For each={props.projects.filter((p) => p.accessible)}>{(p) => <option value={p.id}>{p.name}</option>}</For><option value={NEW}>{tr("New project…")}</option></select></div>
          <Show when={project() === NEW}><div class="form-row"><label for="cl-newp">{tr("New project name")}</label><input id="cl-newp" class="field" maxLength={80} disabled={locked()} value={newName()} onInput={(e) => setNewName(e.currentTarget.value)} /></div></Show>
          <p class="hint">{tr("Uses Git access configured on this computer. The checkout remains available if the session cannot start.")}</p>
          <Show when={error() || view()?.status}><div class="clone-state" classList={{warn: view()?.uncertain, danger: !!error() || (!view()?.busy && !view()?.ready && !view()?.uncertain)}} role="status">{error() || view()?.status}</div></Show>
          <Show when={view()?.busy}><div class="su-progress su-indet" role="progressbar" aria-label={view()?.status} aria-valuemin={0} aria-valuemax={100}><i /></div></Show>
        </div>
        <div class="dlg-foot"><Show when={view()?.uncertain}><button class="btn" disabled={busy()} onClick={() => void step("check")}>{tr("Check result")}</button><button class="btn" disabled={busy()} onClick={() => void step("retire")}>{tr("Choose another destination…")}</button></Show><span class="grow" /><button class="btn" disabled={view()?.cloning && view()?.busy} onClick={() => void close()}>{tr("Cancel")}</button><Show when={view()?.ready} fallback={<button class="btn primary" disabled={!valid() || busy() || view()?.uncertain} onClick={() => void step("begin")}>{tr("Clone repository")}</button>}><button class="btn primary" disabled={!validProject() || busy()} onClick={() => void use()}>{tr("Use checkout")}</button></Show></div>
      </Show>
    </dialog>
    <Show when={browsing()}><FolderBrowserDialog computer={place()!.host} start={parent()} calls={{list: (p, hidden) => bridge.listDirectories(place()!.computer, p, hidden), create: (p) => bridge.createDirectory(place()!.computer, p)}} onChoose={setParent} onClose={() => {setBrowsing(false); queueMicrotask(() => dialog.querySelector<HTMLElement>("#cl-parent")?.focus());}} /></Show>
  </>;
}
