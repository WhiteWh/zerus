import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { clean as cleanPath } from "../sessions/paths";
import { enhance } from "./ProjectCatalogDialog";
import { Icon } from "../lib/icons";
import { FolderBrowserDialog } from "../components/FolderBrowser";
import * as bridge from "../../bridge";
import type { FolderPrompt, Place } from "./project-folders";
import type { Folder } from "../../types";
import { tr } from "../../i18n";

/** setup-folder: approved frame, fields, duplicate message and footer. */
export default function ProjectFolderDialog(props: { prompt: FolderPrompt; busy: boolean; folders: Folder[]; close(): void; save(place: Place, path: string, name: string): void }) {
  const p = props.prompt;
  const initial = p.original ? p.places.find((x) => (x.computer || x.host) === p.original?.machine) : p.places[0];
  const [computer, setComputer] = createSignal(initial?.computer ?? p.original?.machine ?? "");
  const place = () => p.places.find((x) => x.computer === computer());
  const [name, setName] = createSignal(p.original?.name ?? "");
  const [path, setPath] = createSignal(p.original?.path ?? "");
  const [browsing, setBrowsing] = createSignal(false);
  const machine = () => place()?.computer || place()?.host || "";
  const clean = (value: string) => cleanPath(value.trim(), "posix");
  const duplicate = () => props.folders.some((f) => f.key !== p.original?.key && f.machine === machine() && clean(f.path) === clean(path()));
  const valid = () => !!place() && path().trim().startsWith("/") && !/[\r\n\0]/u.test(path()) && !duplicate();
  let dialog!: HTMLDialogElement;
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLElement>("select, input")?.focus()); });
  onCleanup(() => { dialog.close(); queueMicrotask(() => { if (p.origin.isConnected) p.origin.focus(); else document.querySelector<HTMLElement>('.proj-table tr[aria-selected="true"]')?.focus(); }); });
  const browse = async () => {
    if (computer()) setBrowsing(true);
    else { const value = await bridge.pickLocalFolder(path()).catch(() => null); if (value) setPath(value); }
  };
  return <>
    <dialog ref={dialog} class="dlg su-dlg" aria-labelledby="project-folder-title" style={{ width: "560px" }} onCancel={(e) => { if (props.busy) e.preventDefault(); else props.close(); }} onClose={props.close}>
      <div class="dlg-head"><Icon name="folder" size={18} class="accent" /><h2 id="project-folder-title">{p.original ? tr("Edit project folder") : tr("Add project folder")}</h2><span class="grow" /><button class="icon-btn" disabled={props.busy} aria-label={tr("Close")} onClick={props.close}><Icon name="x" size={16} /></button></div>
      <form onSubmit={(e) => { e.preventDefault(); if (!props.busy && valid()) props.save(place()!, path().trim(), name().trim()); }}>
        <div class="dlg-body">
          <div class="form-row"><label for="pf-machine">{tr("Computer")}</label><select ref={enhance} id="pf-machine" class="field su-rsel" value={computer()} disabled={props.busy} onChange={(e) => { setComputer(e.currentTarget.value); setPath(""); }}><Show when={p.original && !initial}><option value={p.original?.machine} disabled>{p.original?.machine}</option></Show><For each={p.places}>{(x) => <option value={x.computer}>{x.label}</option>}</For></select></div>
          <div class="form-row"><label for="pf-name">{tr("Name")}</label><input id="pf-name" class="field" maxLength={80} placeholder={tr("Optional label")} value={name()} disabled={props.busy} onInput={(e) => setName(e.currentTarget.value)} /></div>
          <div class="form-row"><label for="pf-path">{tr("Folder")}</label><div class="row"><input id="pf-path" class="field mono grow" maxLength={32768} placeholder={tr("Absolute folder path")} value={path()} disabled={props.busy} aria-invalid={duplicate()} title={duplicate() ? tr("This folder is already in this project.") : undefined} onInput={(e) => setPath(e.currentTarget.value)} /><button type="button" class="btn" disabled={props.busy || !place()} onClick={() => void browse()}>{tr("Browse…")}</button></div></div>
          <Show when={duplicate()}><div class="form-row"><span /><span class="danger hint">{tr("This folder is already in this project.")}</span></div></Show>
        </div>
        <div class="dlg-foot"><span class="grow" /><button type="button" class="btn" disabled={props.busy} onClick={props.close}>{tr("Cancel")}</button><button type="submit" class="btn primary" disabled={props.busy || !valid()}>{p.original ? tr("Save folder") : tr("Add folder")}</button></div>
      </form>
    </dialog>
    <Show when={browsing()}><FolderBrowserDialog computer={machine()} start={path()} calls={{ list: (value, hidden) => bridge.listDirectories(computer(), value, hidden), create: (value) => bridge.createDirectory(computer(), value) }} onChoose={setPath} onClose={() => { setBrowsing(false); queueMicrotask(() => dialog.querySelector<HTMLElement>("#pf-path")?.focus()); }} /></Show>
  </>;
}
