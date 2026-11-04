import { describe, expect, it } from "vitest";
import { applyMotionAttributes, motionAttributes, type MotionInputs } from "./motion";

const base: MotionInputs = { animateActivity: true, reduceMotion: false, systemReduced: false, hidden: false };

describe("motionAttributes", () => {
  it("sets nothing with the Qt defaults on a visible window", () => {
    expect(motionAttributes(base)).toEqual({});
  });

  it("marks Animate working sessions off on its own", () => {
    expect(motionAttributes({ ...base, animateActivity: false })).toEqual({ animateActivity: "0" });
  });

  it("reduces motion for the setting or the system preference", () => {
    expect(motionAttributes({ ...base, reduceMotion: true })).toEqual({ reduceMotion: "1" });
    expect(motionAttributes({ ...base, systemReduced: true })).toEqual({ reduceMotion: "1" });
    expect(motionAttributes({ ...base, reduceMotion: true, systemReduced: true })).toEqual({ reduceMotion: "1" });
  });

  it("pauses everything while the window is hidden, whatever the settings", () => {
    expect(motionAttributes({ ...base, hidden: true })).toEqual({ pageHidden: "1" });
    expect(motionAttributes({ animateActivity: false, reduceMotion: true, systemReduced: false, hidden: true })).toEqual({ animateActivity: "0", reduceMotion: "1", pageHidden: "1" });
  });

  it("marks Linux focus loss separately so visible entrances keep running", () => {
    expect(motionAttributes({ ...base, unfocused: true })).toEqual({ motionUnfocused: "1" });
    expect(motionAttributes({ ...base, hidden: true, unfocused: true })).toEqual({ pageHidden: "1", motionUnfocused: "1" });
  });
});

describe("applyMotionAttributes", () => {
  it("adds and removes only the motion attributes", () => {
    const dataset: Record<string, string | undefined> = { skin: "zerus-dark" };
    applyMotionAttributes(dataset, { reduceMotion: "1", pageHidden: "1" });
    expect(dataset).toEqual({ skin: "zerus-dark", reduceMotion: "1", pageHidden: "1" });
    applyMotionAttributes(dataset, { animateActivity: "0" });
    expect(dataset).toEqual({ skin: "zerus-dark", animateActivity: "0" });
    applyMotionAttributes(dataset, {});
    expect(dataset).toEqual({ skin: "zerus-dark" });
  });

  it("does not write an attribute that already has its value", () => {
    const writes: string[] = [];
    const values: Record<string, string | undefined> = { reduceMotion: "1" };
    const dataset = new Proxy(values, { set(target, key, value) { writes.push(String(key)); target[key as string] = value; return true; } });
    applyMotionAttributes(dataset, { reduceMotion: "1" });
    expect(writes).toEqual([]);
  });
});
