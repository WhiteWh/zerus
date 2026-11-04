import { createEffect, createSignal } from "solid-js";
import { ZERUS_PETAL } from "../lib/qt-icons";
import type { LogoMotion } from "../../store/ui";
import { motionPaused } from "../../store/visibility";
import { motionReduced } from "../motion";
import "../styles/logo.css";

// The rail's Zerus mark (tray/src/SwarmButton.h). While any session needs
// attention it takes the attention colour and turns; otherwise it rests in the
// neutral colour. The mark is one layer that only rotates (and, in the Qt
// variant, fades), so the compositor runs it at the display rate without
// repainting. Motion stops for Reduce motion and pauses while the window is
// hidden (styles/motion.css); when attention clears, the current turn
// finishes and the mark settles.

const PETALS = [0, 120, 240]
  .map((deg) => `<path transform="rotate(${deg} 50 50) translate(56 30) scale(.88) translate(-56 -30)" d="${ZERUS_PETAL}"/>`)
  .join("");
const MARK = `<g transform="translate(50 50) scale(1.06) translate(-50 -50)">${PETALS}</g>`;

export function ZerusLogo(props: { attention: boolean; motion: LogoMotion; size?: number }) {
  const [running, setRunning] = createSignal(props.attention);

  // Start at once; stop only at the end of a cycle, where the mark is at rest.
  // Reduced or paused (hidden window) motion has no cycle end to wait for.
  createEffect(() => {
    if (props.attention) setRunning(true);
    else if (motionReduced() || motionPaused()) setRunning(false);
  });
  const onIteration = (e: AnimationEvent) => {
    if (!props.attention && (e.target as Element).classList.contains("zl-clock")) setRunning(false);
  };

  return (
    <span class="zlogo" style={{ "--zl-size": `${props.size ?? 28}px` }}
      data-motion={props.motion} data-attention={props.attention ? "1" : "0"} data-running={running() ? "1" : "0"}
      onAnimationIteration={onIteration} aria-hidden="true">
      <span class="zl-mark">
        <svg class="zl-fade" viewBox="0 0 100 100" fill="currentColor" innerHTML={MARK} />
      </span>
      {/* An invisible element carries the 3 s cycle so the stop always lands on a cycle boundary. */}
      <i class="zl-clock" />
    </span>
  );
}
