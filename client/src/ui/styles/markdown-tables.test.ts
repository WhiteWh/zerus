import { expect, it } from "vitest";

const fs = (await import(/* @vite-ignore */ ["node", "fs"].join(":"))) as {
  readFileSync(path: URL, encoding: "utf8"): string;
};
const css = fs.readFileSync(new URL("./detail.css", import.meta.url), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
const declarations = (selector: string) => {
  const rule = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].find((m) => m[1].trim() === selector);
  if (!rule) throw Error(`Missing table rule: ${selector}`);
  return Object.fromEntries(rule[2].split(";").filter((s) => s.includes(":")).map((s) => s.split(":").map((v) => v.trim())));
};

it("keeps whole-word min-content widths in short message table columns while wrapping prose", () => {
  const cells = declarations(".act-table th, .act-table td");
  // Unlike anywhere, break-word's emergency breaks do not shrink min-content
  // to one character when the automatic table algorithm allocates columns.
  expect(cells["overflow-wrap"]).toBe("break-word");
  expect(cells["white-space"]).toBe("normal");
  expect(cells["word-break"] ?? "normal").toBe("normal");
  expect(declarations(".act-table")["table-layout"] ?? "auto").toBe("auto");
});

it("keeps wide message tables inside their own horizontal scroller", () => {
  expect(declarations(".act-table-wrap")).toMatchObject({ "max-width": "100%", "overflow-x": "auto" });
  expect(declarations(".act-table")["min-width"]).toContain("var(--table-columns)");
});
