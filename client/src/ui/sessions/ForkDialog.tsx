import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { sessionForks } from "../../store/sessionForks";
import type { Details } from "../../types";
import { Icon } from "../lib/icons";
import type { ActionRow } from "./actions-ui";
import { createConfirm } from "./newsession-view";
import { LaunchWarning } from "./LaunchWarning";
import { tr } from "../../i18n";

/** Mockup SessionDialogs.Fork; capability and submission semantics from Qt. */
export default function ForkDialog(props: { row: ActionRow; details: Details; close(): void }) {
  const [tag, setTag] = createSignal(sessionForks.suggested(props.row));
  const confirm = createConfirm(), controller = new AbortController();
  let input!: HTMLInputElement;
  onMount(() => queueMicrotask(() => { input.focus(); input.select(); }));
  onCleanup(() => { controller.abort(); confirm.cancel(); });
  const problem = () => sessionForks.problem(props.row, tag());
  const submit = (event: Event) => {
    event.preventDefault();
    if (problem()) return;
    void sessionForks.submit(props.row, props.details, tag(), controller.signal, confirm.ask, props.close);
  };
  return <>
    <form onSubmit={submit}>
      <div class="dlg-head"><Icon name="git-fork" size={18} class="accent" /><h2 id="action-dlg-title">{tr("Fork session")}</h2><span class="grow" />
        <button type="button" class="icon-btn" title={tr("Close")} aria-label={tr("Close")} onClick={props.close}><Icon name="x" size={15} /></button></div>
      <div class="dlg-body">
        <p class="muted">{tr("The fork continues this conversation in a new session. The original keeps running.")}</p>
        <div class="form-row"><label for="fork-tag">{tr("New session name")}</label>
          <input ref={input} id="fork-tag" class="field" data-initial autofocus value={tag()} disabled={sessionForks.working()} onInput={(event) => setTag(event.currentTarget.value)} /></div>
        <Show when={problem()}><div class="hint dlg-error">{problem()}</div></Show>
      </div>
      <div class="dlg-foot"><span class="spacer" /><button type="button" class="btn" onClick={props.close}>{tr("Cancel")}</button>
        <button type="submit" class="btn primary" disabled={!!problem() || sessionForks.busy()}>{tr("Fork")}</button></div>
    </form>
    <LaunchWarning confirm={confirm} />
  </>;
}
