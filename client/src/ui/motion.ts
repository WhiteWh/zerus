import { createRenderEffect, createRoot, createSignal } from "solid-js";
import { flag } from "../store/settings";
import { motionUnfocused, pageHidden } from "../store/visibility";

// The motion settings on the document root, where styles/motion.css stops or
// pauses every endless animation:
//
//   data-animate-activity="0"  Animate working sessions is off
//   data-reduce-motion="1"     Reduce motion is on, or the system asks for reduced motion
//   data-page-hidden="1"       the document is hidden
//   data-motion-unfocused="1"  Linux toplevel focus is elsewhere; only endless motion pauses

export interface MotionInputs {
  animateActivity: boolean;
  reduceMotion: boolean;
  /** `prefers-reduced-motion: reduce`. */
  systemReduced: boolean;
  hidden: boolean;
  unfocused?: boolean;
}

/** Root `data-*` attributes (dataset names); an absent one is removed. */
export interface MotionAttributes {
  animateActivity?: "0";
  reduceMotion?: "1";
  pageHidden?: "1";
  motionUnfocused?: "1";
}

const ATTRIBUTES = ["animateActivity", "reduceMotion", "pageHidden", "motionUnfocused"] as const;

export function motionAttributes(m: MotionInputs): MotionAttributes {
  const attributes: MotionAttributes = {};
  if (!m.animateActivity) attributes.animateActivity = "0";
  if (m.reduceMotion || m.systemReduced) attributes.reduceMotion = "1";
  if (m.hidden) attributes.pageHidden = "1";
  if (m.unfocused) attributes.motionUnfocused = "1";
  return attributes;
}

/** Writes `attributes` into a dataset, touching only the attributes that change. */
export function applyMotionAttributes(dataset: Record<string, string | undefined>, attributes: MotionAttributes) {
  for (const name of ATTRIBUTES) {
    const value = attributes[name];
    if (value === undefined) { if (name in dataset) delete dataset[name]; }
    else if (dataset[name] !== value) dataset[name] = value;
  }
}

const query = typeof matchMedia === "undefined" ? undefined : matchMedia("(prefers-reduced-motion: reduce)");
const [systemReduced, setSystemReduced] = createSignal(query?.matches ?? false);
query?.addEventListener("change", (e) => setSystemReduced(e.matches));

/** Reactive: motion is reduced (the setting or the system), for motion driven from script. */
export const motionReduced = () => flag("workspace/reduceMotion") || systemReduced();

/** Keeps the root's motion attributes in step with the settings, the system and the window. */
export function startMotion(root: HTMLElement = document.documentElement) {
  createRoot(() => {
    createRenderEffect(() => applyMotionAttributes(root.dataset, motionAttributes({
      animateActivity: flag("workspace/animateActivity"),
      reduceMotion: flag("workspace/reduceMotion"),
      systemReduced: systemReduced(),
      hidden: pageHidden(),
      unfocused: motionUnfocused(),
    })));
  });
}
