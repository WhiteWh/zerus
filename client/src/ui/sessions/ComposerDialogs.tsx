import { For, Show, createEffect } from "solid-js";
import { Icon } from "../lib/icons";
import { composer, type Dialog } from "../../store/composer";
import type { ClearControl, ColdPrompt, StartQuestion } from "../../types";
import { tr } from "../../i18n";
import { trText } from "../../text";

// The composer's questions, each a modal over the shared dialog styles
// (ActionDialogs.tsx): "Continue with a cold cache?" (SessionsWindow.cpp
// 4815-4836), "Start the session to send?" (4706-4730) and "Clear session?"
// (4637-4656). Cancel has the focus and is the default, and Escape is Cancel
// (Qt's setDefaultButton(cancel), setEscapeButton(cancel)). Texts are plain
// text: reported names stay literal.

function ColdDialog(props: { prompt: ColdPrompt }) {
  return (
    <>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="composer-dlg-title">{props.prompt.title}</h2></div>
      <div class="dlg-body">
        <div class="confirm-body" id="composer-dlg-body" style={{ "white-space": "pre-wrap" }}>{props.prompt.text}</div>
        <Show when={props.prompt.informative}><div class="dlg-detail" style={{ "white-space": "pre-wrap" }}>{props.prompt.informative}</div></Show>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <For each={props.prompt.choices}>{(choice) => (
          <button type="button" class="btn" disabled={!choice.enabled} title={choice.tooltip || undefined}
            onClick={() => { void composer.answer({ kind: "cold", action: choice.action }); }}>{choice.label}</button>
        )}</For>
        <button type="button" class="btn primary" data-initial onClick={() => composer.closeDialog()}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/** The core's prose, translated here: the session name and context usage stay verbatim. */
function StartDialog(props: { question: StartQuestion }) {
  return (
    <>
      <div class="dlg-head"><Icon name="circle-question-mark" size={18} class="accent" /><h2 id="composer-dlg-title">{trText(props.question.title)}</h2></div>
      <div class="dlg-body">
        <div class="confirm-body" id="composer-dlg-body" style={{ "white-space": "pre-wrap" }}>{trText(props.question.text)}</div>
        <Show when={props.question.informative.source}><div class="dlg-detail" style={{ "white-space": "pre-wrap" }}>{trText(props.question.informative)}</div></Show>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <For each={props.question.options}>{(option) => (
          <button type="button" class="btn" disabled={!option.enabled} title={option.tooltip ? trText(option.tooltip) : undefined}
            onClick={() => { void composer.answer({ kind: "start", choice: option.choice }); }}>{trText(option.label)}</button>
        )}</For>
        <button type="button" class="btn primary" data-initial onClick={() => composer.closeDialog()}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/** The snapshot taken when it opened: the confirm sends that identity back, and Rust refuses it quietly once it changed. */
function ClearDialog(props: { control: ClearControl }) {
  return (
    <>
      <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="composer-dlg-title">{props.control.title}</h2></div>
      <div class="dlg-body"><div class="confirm-body" id="composer-dlg-body" style={{ "white-space": "pre-wrap" }}>{props.control.text}</div></div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn solid-danger" onClick={() => { void composer.confirmClear(); }}>{props.control.confirm}</button>
        <button type="button" class="btn primary" data-initial onClick={() => composer.closeDialog()}>{tr("Cancel")}</button>
      </div>
    </>
  );
}

/**
 * One modal host for the composer's dialogs. It opens with the store's
 * dialog and focuses Cancel; any way out but a choice is a Cancel. A dialog
 * that follows another (Clear context from the cold question) reuses it.
 */
export function ComposerDialogs(props: { onClosed?: () => void }) {
  let el: HTMLDialogElement | undefined;
  createEffect(() => {
    const d = composer.dialog();
    if (!el) return;
    if (!d) { if (el.open) el.close(); return; }
    if (!el.open) el.showModal();
    queueMicrotask(() => el?.querySelector<HTMLElement>("[data-initial]")?.focus());
  });
  const body = (d: Dialog) => (d.kind === "cold" ? <ColdDialog prompt={d.prompt} /> : d.kind === "start" ? <StartDialog question={d.question} /> : <ClearDialog control={d.control} />);
  return (
    <dialog class="dlg" ref={el} role="alertdialog" aria-labelledby="composer-dlg-title" aria-describedby="composer-dlg-body" style={{ width: "520px" }}
      onCancel={(e) => { e.preventDefault(); composer.closeDialog(); }}
      // A close by other means answers Cancel; the close of a dialog the store already ended (or replaced) does nothing.
      onClose={() => {
        if (el?.open) return;
        if (composer.dialog()) composer.closeDialog();
        props.onClosed?.();
      }}>
      <Show when={composer.dialog()} keyed>{body}</Show>
    </dialog>
  );
}
