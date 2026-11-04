import { describe, expect, it } from "vitest";
import { resolveSkins, type SkinFile } from "./skin";
import dark from "./skins/zerus-dark.json";
import light from "./skins/zerus-light.json";
import fjord from "./skins/fjord.json";
import paper from "./skins/paper.json";

function luminance(hex: string): number {
  const [r, g, b] = hex.slice(1).match(/../g)!.map((part) => {
    const channel = parseInt(part, 16) / 255;
    return channel <= .04045 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4;
  });
  return .2126 * r + .7152 * g + .0722 * b;
}

describe("destructive button readability", () => {
  for (const skin of resolveSkins([dark, light, fjord, paper] as SkinFile[])) {
    it(`keeps normal-size danger text readable in ${skin.id}`, () => {
      const color = skin.tokens.color;
      const values = [luminance(String(color.danger)), luminance(String(color["danger-text"]))];
      expect((Math.max(...values) + .05) / (Math.min(...values) + .05)).toBeGreaterThanOrEqual(4.5);
    });
  }
});
