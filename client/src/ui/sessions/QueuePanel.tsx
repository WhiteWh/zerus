import { For, Show, createMemo } from "solid-js";
import { composer } from "../../store/composer";
import { activity } from "../../store/session";
import { selected, subagentOpen } from "../../store/ui";
import { chord } from "../../store/shortcuts";
import { tr } from "../../i18n";
import { Icon } from "../lib/icons";
import { OutgoingMessage } from "./Activity";
import { queuePresentation } from "./queue-message";

/** Native pending messages remain distinct from the sent Activity. */
export function QueuePanel() {
  const display = createMemo(() => {
    const queue = composer.queue();
    return queue ? queuePresentation(queue, selected(), composer.view(), activity.target(), activity.summary(), !!subagentOpen()) : undefined;
  }, undefined, { equals: (a, b) => JSON.stringify(a?.items) === JSON.stringify(b?.items)
    && JSON.stringify(a?.scope) === JSON.stringify(b?.scope) });
  const tooltip = () => {
    const hint = composer.queue()?.hint ?? "", keys = chord("queue.sendNow");
    // Keep the existing Send now guard and customized shortcut.
    return keys ? hint ? `${hint} (${keys})` : keys : hint;
  };
  return (
    <Show when={composer.queue()}>{(q) => (
      <div class="act-queue" role="region" aria-label={tr("Queued in agent")}>
        <div class="act-queue-head">
          <Icon name="clock" size={15} /><b>{tr("Queued in agent")}</b><span class="grow" />
          <button type="button" class="btn small" disabled={!q().enabled} title={tooltip()}
            onClick={() => { void composer.sendNow(q().id); }}>{q().label}</button>
        </div>
        <div class="act-queue-text">
          <For each={display()?.items}>{(item) => <div class="act-queue-message">
            <OutgoingMessage item={item} scope={display()?.scope} session={display()?.scope ? selected() : undefined} />
          </div>}</For>
        </div>
      </div>
    )}</Show>
  );
}
