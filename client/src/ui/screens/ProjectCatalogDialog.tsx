import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { catalogName, type ProjectPrompt } from "./project-catalog";
import { projectColor } from "./projects-ui";
import { tr } from "../../i18n";

// The mockup's RichSelect enhancement; WebKit's native fallback keeps the
// same readable text and keyboard behavior.
export function enhance(select: HTMLSelectElement) {
  if (!CSS.supports("appearance", "base-select")) return;
  const button = document.createElement("button");
  button.type = "button";
  button.append(document.createElement("selectedcontent"));
  select.prepend(button);
}

/** setup-new-project / setup-delete-project, with the mockup's sizes and texts. */
export default function ProjectCatalogDialog(props: {
  prompt: ProjectPrompt;
  enabled: boolean;
  close(): void;
  create(name: string): void;
  remove(destination: string): void;
}) {
  const prompt = props.prompt;
  const [value, setValue] = createSignal(prompt.kind === "delete" ? prompt.selected : "");
  let dialog!: HTMLDialogElement;
  const confirm = () => {
    if (!props.enabled) return;
    if (prompt.kind === "new" && catalogName(value())) props.create(value());
    else if (prompt.kind === "delete" && value()) props.remove(value());
  };
  const submit = (event: Event) => {
    event.preventDefault();
    if (prompt.kind === "delete") props.close();
    else confirm();
  };
  onMount(() => {
    dialog.showModal();
    queueMicrotask(() => dialog.querySelector<HTMLElement>("[data-initial]")?.focus());
  });
  onCleanup(() => { dialog.close(); if (prompt.origin.isConnected) prompt.origin.focus(); });
  return <dialog ref={dialog} class="dlg su-dlg" aria-labelledby="project-catalog-title"
    style={{ width: prompt.kind === "new" ? "420px" : "500px" }}
    onCancel={props.close} onClose={props.close}>
    <form onSubmit={submit}>
      <div class="dlg-head"><Icon name={prompt.kind === "new" ? "folder" : "triangle-alert"} size={18} class="accent" />
        <h2 id="project-catalog-title">{prompt.kind === "new" ? tr("New project") : tr("Delete project")}</h2><span class="grow" />
        <button type="button" class="icon-btn" aria-label={tr("Close")} onClick={props.close}><Icon name="x" size={16} /></button>
      </div>
      <div class="dlg-body">
        <Show when={prompt.kind === "delete" ? prompt : undefined} fallback={
          <div class="form-row"><label for="np-name">{tr("Name")}</label><input id="np-name" class="field" maxLength={80} data-initial value={value()} onInput={(event) => setValue(event.currentTarget.value)} /></div>
        }>{(p) => <>
          <p class="confirm-body">{p().text}</p>
          <select ref={enhance} class="field su-rsel" aria-label={tr("Project that receives the sessions")} value={value()} onChange={(event) => setValue(event.currentTarget.value)}>
            <For each={p().options}>{(option) => <option value={option.id} aria-label={`${option.name}${option.is_default ? " Default" : ""}`}>
              <span class="su-rssw entity" style={{ "--entity": projectColor(option) }}><Icon name="folder" size={11} /></span>
              <span class="su-rslabel">{option.name}</span><Show when={option.is_default}><span class="su-rsdetail">{tr("Default")}</span></Show>
            </option>}</For>
          </select>
        </>}</Show>
      </div>
      <div class="dlg-foot"><span class="grow" />
        <Show when={prompt.kind === "delete"} fallback={<>
          <button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button>
          <button type="submit" class="btn primary" disabled={!props.enabled || !catalogName(value())}>OK</button>
        </>}>
          <button type="button" class="btn solid-danger" disabled={!props.enabled || !value()} onClick={confirm}>{tr("Move sessions and delete")}</button>
          <button type="button" class="btn primary" data-initial autofocus onClick={props.close}>{tr("Cancel")}</button>
        </Show>
      </div>
    </form>
  </dialog>;
}
