import { Show, createEffect, onCleanup, onMount, untrack } from "solid-js";
import { nativeUi } from "../../store/native-ui";
import { selected, setDetailTab } from "../../store/ui";
import { questions } from "../../store/questions";
import { pageHidden } from "../../store/visibility";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

/** design/webui screens/sessions/SessionDetail NativeTab. */
export function NativeUiTab() {
  let frame!: HTMLDivElement;
  let scheduled = 0;
  const measure = () => {
    cancelAnimationFrame(scheduled);
    scheduled = requestAnimationFrame(() => {
      const rect = frame.getBoundingClientRect();
      nativeUi.rectangle({ x: rect.x + 1, y: rect.y + 1, width: rect.width - 2, height: rect.height - 2 });
    });
  };
  onMount(() => {
    const observer = new ResizeObserver(measure);
    observer.observe(frame);
    window.addEventListener("resize", measure);
    document.addEventListener("scroll", measure, true);
    measure();
    onCleanup(() => { observer.disconnect(); cancelAnimationFrame(scheduled); window.removeEventListener("resize", measure); document.removeEventListener("scroll", measure, true); nativeUi.opened(false); });
  });
  // A hidden window retains this component. Reopening clears the old native
  // bounds even when the DOM size did not change, so measure on visibility
  // as well as on a new exact request.
  createEffect(() => { nativeUi.request(); pageHidden(); untrack(measure); });
  return <div class="native-tab">
    <div class="term-bar"><b>{tr("DeepSeek workspace / %1", selected()?.machine ?? "")}</b><span class="grow" />
      <Show when={questions.view()}><button class="btn small" onClick={() => setDetailTab("activity")}>{tr("Respond in Activity")}</button></Show>
      <button class="icon-btn" title={tr("Reload")} aria-label={tr("Reload native UI")} disabled={nativeUi.status().status === "offline"} onClick={() => nativeUi.reload()}><Icon name="refresh-cw" size={15} /></button>
    </div>
    <div class="native-frame" ref={frame}>
      <Show when={nativeUi.status().status !== "ready"}>
        <div class="empty" role="status"><Icon name="monitor-smartphone" size={30} /><h3>{tr("Agent's own web UI")}</h3><p>{nativeUi.status().text}</p></div>
      </Show>
    </div>
  </div>;
}
