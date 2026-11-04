import { For, Show, onCleanup, onMount } from "solid-js";
import { sessionModels as models } from "../../store/sessionModels";
import { terminal } from "../../store/terminal";
import { setDetailTab } from "../../store/ui";
import { ModelList } from "./ModelPicker";
import { Icon } from "../lib/icons";
import "../styles/newsession.css";
import { tr } from "../../i18n";

function SettingsDialog(props: { anchor: () => HTMLButtonElement | undefined }) {
  let dialog!: HTMLDialogElement;
  const v = () => models.view()!;
  const native = () => v().terminal && !v().enabled;
  onMount(() => {
    dialog.showModal();
    (dialog.querySelector<HTMLElement>(native() ? ".cmp-settings-terminal" : "input,select") ?? dialog.querySelector<HTMLElement>("button"))?.focus();
  });
  onCleanup(() => {
    if (dialog.open) dialog.close();
    queueMicrotask(() => props.anchor()?.focus());
  });
  const outside = (e: PointerEvent) => {
    if (e.target !== dialog) return;
    const box = dialog.getBoundingClientRect();
    if (e.clientX < box.left || e.clientX > box.right || e.clientY < box.top || e.clientY > box.bottom) models.close();
  };
  const choices = () => {
    const all = models.options();
    return all.some((m) => m.id === models.model()) || !models.model() ? all
      : [{ id: models.model(), label: models.model(), effort_options: [], effort_labels: [] }, ...all];
  };
  const effortEnabled = () => v().enabled && models.options().some((m) => m.id === models.model())
    && (v().openCode ? models.efforts().length > 1 : !!models.effort());
  const openTerminal = () => { models.close(); terminal.opened(); setDetailTab("terminal"); };
  return (
    <dialog ref={dialog} class="dlg cmp-settings-dialog" aria-labelledby="cmp-settings-title" onPointerDown={outside}
      onCancel={(e) => { e.preventDefault(); models.close(); }}>
      <div class="dlg-head"><Icon name="sparkles" size={18} class="accent" /><h2 id="cmp-settings-title">{tr("Model and effort")}</h2><span class="grow" />
        <button type="button" class="icon-btn" aria-label={tr("Close")} title={tr("Close")} onClick={() => models.close()}><Icon name="x" size={16} /></button>
      </div>
      <div class="dlg-body">
        <Show when={!native()}>
          <div class="form-row"><label for="session-model-choice">{tr("Model")}</label><Show when={v().openCode} fallback={
            <select id="session-model-choice" class="field" aria-label={tr("Model")} value={models.model()} disabled={!v().enabled || choices().length < 2}
              onChange={(e) => models.chooseModel(e.currentTarget.value)}>
              <For each={choices()}>{(m) => <option value={m.id} selected={m.id === models.model()}>{m.label}</option>}</For>
            </select>
          }>
            <fieldset disabled={!v().enabled} class="cmp-settings-models" aria-label={tr("Model")}>
              <ModelList view={models.picker} act={models.pickerActions} disabled={!v().enabled} />
            </fieldset>
          </Show></div>
          <div class="form-row"><label for="session-effort-choice">{tr("Reasoning effort")}</label>
            <select id="session-effort-choice" class="field" aria-label={tr("Reasoning effort")} value={models.effort()} disabled={!effortEnabled()}
              onChange={(e) => models.chooseEffort(e.currentTarget.value)}>
              <For each={models.efforts()}>{(e) => <option value={e.value} selected={e.value === models.effort()}>{e.label}</option>}</For>
            </select>
          </div>
        </Show>
        <p class="cmp-settings-hint hint" role="status">{v().hint}<Show when={models.catalogError()}>{`\n${models.catalogError()}`}</Show></p>
        <Show when={v().changeNotice}><div class="notice warn cmp-settings-notice"><Icon name="info" size={14} /><div>{v().changeNotice}</div></div></Show>
      </div>
      <div class="dlg-foot">
        <button type="button" class="btn ghost cmp-settings-terminal" onClick={openTerminal}>{tr("Open Terminal")}</button>
        <span class="spacer" />
        <button type="button" class="btn" title={tr("Close without applying changes")} onClick={() => models.close()}>{tr("Cancel")}</button>
        <Show when={!native()}><button type="button" class="btn primary" disabled={!models.canApply()} onClick={() => { void models.apply(); }}>{v().applyLabel}</button></Show>
      </div>
    </dialog>
  );
}

export function ModelSettingsButton() {
  let anchor: HTMLButtonElement | undefined;
  return <Show when={models.view()}>{(v) => <>
    <button ref={anchor} type="button" class="btn ghost cmp-model" aria-label={v().accessible} title={v().tooltip}
      aria-haspopup="dialog" aria-expanded={models.opened()} onClick={() => models.open()}>
      <span class="ell cmp-model-name">{v().shownModel}</span><span class="dim cmp-model-suffix">{v().suffix}</span><Icon name="chevron-down" size={12} />
    </button>
    <Show when={models.opened()}><SettingsDialog anchor={() => anchor} /></Show>
  </>}</Show>;
}
