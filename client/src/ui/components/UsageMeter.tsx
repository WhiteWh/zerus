import { For, Show } from "solid-js";
import { current, countdown, pace, period, windowText, type UsageWindow } from "../lib/usage";
import { tr } from "../../i18n";

// Limit meters, ported from AccountUsage::Button::paintContents (tray/src/AccountUsage.h).
// Per window: a meter (34 px, 20 compact), 6 px, the "5h 34%" text in the pace
// colour, then 16 px (10 compact) to the next window. Rails draw two thin bars:
// usage (3 px, pace colour) above the elapsed share of the window (2 px, grey).
// The marker variants draw one 4 px bar with a 1.5 px elapsed-time marker; usage
// beyond that marker takes the pace colour. Without a window length or reset
// time every variant draws the plain usage bar. Stale windows show only the
// track and "5h refresh"; loading shows tracks and text placeholders.

export type UsageIndicator = "rails" | "marker" | "marker-countdown";

let clipId = 0;

const toneVar = (tone: string) => `var(--z-list-usage-${tone})`;

function Meter(props: { w?: UsageWindow; width: number; indicator: UsageIndicator }) {
  const id = `um-${++clipId}`;
  const fresh = () => !!props.w && current(props.w);
  const p = () => (props.w && fresh() ? pace(props.w) : undefined);
  const W = () => props.width;
  return (
    <svg class="um-bar" width={W()} height="22" viewBox={`0 0 ${W()} 22`} aria-hidden="true">
      <Show when={p()?.known && props.indicator === "rails"} fallback={
        <>
          <clipPath id={id}><rect x="0" y="9" width={W()} height="4" rx="2" /></clipPath>
          <rect class="um-track" x="0" y="9" width={W()} height="4" rx="2" />
          <Show when={p()}>{(s) => (
            <g clip-path={`url(#${id})`}>
              <Show when={s().known && s().used < 100 && !s().early && s().used > s().elapsed * 100} fallback={
                <rect x="0" y="9" width={(W() * s().used) / 100} height="4" fill={toneVar(s().tone)} />
              }>
                <rect x="0" y="9" width={W() * s().elapsed} height="4" fill={toneVar("normal")} />
                <rect x={W() * s().elapsed} y="9" width={(W() * (s().used - s().elapsed * 100)) / 100} height="4" fill={toneVar(s().tone)} />
              </Show>
            </g>
          )}</Show>
          <Show when={p()?.known}>
            <rect class="um-marker" x={W() * p()!.elapsed - 0.75} y="6" width="1.5" height="10" rx="0.75" />
          </Show>
        </>
      }>
        <rect class="um-track" x="0" y="7" width={W()} height="3" rx="1.5" />
        <rect class="um-track" x="0" y="12" width={W()} height="2" rx="1" />
        <rect x="0" y="7" width={(W() * p()!.used) / 100} height="3" rx="1.5" fill={toneVar(p()!.tone)} />
        <rect class="um-elapsed" x="0" y="12" width={W() * p()!.elapsed} height="2" rx="1" />
      </Show>
    </svg>
  );
}

/** The meters for one account. `summary` replaces them when the provider reports no windows; `resetFrom` (default 100) is the percent from which the text adds the time to reset. */
export function UsageMeters(props: { windows: UsageWindow[]; loading?: boolean; compact?: boolean; indicator?: UsageIndicator; summary?: string; resetFrom?: number }) {
  const indicator = () => props.indicator ?? "rails";
  const bar = () => (props.compact ? 20 : 34);
  const slots = () => (props.loading ? [undefined, undefined] : props.windows.slice(0, 2));
  const countdownShown = (w?: UsageWindow) => indicator() === "marker-countdown" && !props.compact && !!w && current(w) && w.resetsIn !== null;
  return (
    <span class="usage-meters" classList={{ compact: props.compact }}>
      <Show when={props.loading || props.windows.length} fallback={<span class="um-summary">{props.summary ?? tr("limits n/a")}</span>}>
        <For each={slots()}>{(w) => {
          const fresh = () => !!w && current(w);
          const tone = () => (w && fresh() ? pace(w).tone : undefined);
          return (
            <span class="um-window">
              <Meter w={w} width={bar()} indicator={indicator()} />
              <Show when={w} fallback={<i class="um-skeleton" />}>
                {/* Percent changes keep a stable slot ("5h 100%"); only a reset countdown widens it. */}
                <span class="um-text tabular" style={{ color: tone() ? toneVar(tone()!) : undefined, "min-width": props.compact ? undefined : `${period(w!).length + 5}ch` }}>
                  {windowText(w!, !countdownShown(w), props.resetFrom)}
                </span>
                <Show when={countdownShown(w)}>
                  <span class="um-countdown tabular">
                    <svg width="8" height="22" viewBox="0 0 8 22" aria-hidden="true"><path d="M.5 6.5H6.5L3.5 11L6.5 15.5H.5L3.5 11Z" /></svg>
                    {countdown(w!.resetsIn!)}
                  </span>
                </Show>
              </Show>
            </span>
          );
        }}</For>
      </Show>
    </span>
  );
}
