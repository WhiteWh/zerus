import { Show } from "solid-js";
import { Icon } from "../lib/icons";
import { OS_GLYPH, OS_NAME, type OsKind } from "../lib/os-icons";
import { trNoop, tr } from "../../i18n";

// A machine's operating system in a rounded tile, with its connection state as
// a dot on the tile's corner (the prototype's machine list idea). Connected is
// green, not connected red, connecting amber with a pulsing ring (transform and
// opacity only), disabled grey. An unknown system shows the generic computer.

export type MachineState = "online" | "offline" | "connecting" | "disabled";

const STATE_LABEL: Record<MachineState, string> = { online: "Connected", offline: trNoop("Not connected"), connecting: "Connecting", disabled: "Disabled" };

export function MachineTile(props: { os?: OsKind; state: MachineState; size?: number; title?: string }) {
  const size = () => props.size ?? 34;
  return (
    <span class="mtile" classList={{ [`s-${props.state}`]: true }} style={{ "--mt": `${size()}px` }} role="img"
      aria-label={`${props.os ? OS_NAME[props.os] : tr("Computer")}, ${tr(STATE_LABEL[props.state])}`} title={props.title ?? `${props.os ? OS_NAME[props.os] : tr("Unknown system")}\n${tr(STATE_LABEL[props.state])}`}>
      <Show when={props.os} fallback={<Icon name="machines" size={Math.round(size() * 0.53)} />}>
        <svg width={Math.round(size() * 0.53)} height={Math.round(size() * 0.53)} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" innerHTML={OS_GLYPH[props.os!]} />
      </Show>
      <i class="mtile-dot" />
    </span>
  );
}
