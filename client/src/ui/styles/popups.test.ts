import { describe, expect, it } from "vitest";

// Guards for the popup scrollbar rule (base.css). WebKitGTK without accelerated
// compositing paints every overlay scrollbar above the whole page, menus and
// popovers included; while a popup is open, the scroll containers outside it
// hide their scrollbars. The rule names both sides, so both lists must stay
// complete: every scroll container in the stylesheets and every popup root in
// the components.

// Read from disk, as motion.test.ts does: the specifier is built so TypeScript
// (no Node types here) leaves it alone.
const fs = (await import(/* @vite-ignore */ ["node", "fs"].join(":"))) as {
  readdirSync(path: URL, options?: { recursive: true }): string[];
  readFileSync(path: URL, encoding: "utf8"): string;
};
const here = new URL("./", import.meta.url);
const src = new URL("../../", import.meta.url);
const strip = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, "");
const sheets = fs.readdirSync(here).filter((f) => f.endsWith(".css")).map((f) => ({ file: f, css: strip(fs.readFileSync(new URL(f, here), "utf8")) }));

/** Leaf rules (no nested blocks) of a stylesheet. */
function rules(css: string): { prelude: string; body: string }[] {
  const out: { prelude: string; body: string }[] = [];
  const stack: { prelude: string; start: number; nested: boolean }[] = [];
  let last = 0;
  for (let i = 0; i < css.length; i++) {
    if (css[i] === "{") {
      if (stack.length) stack[stack.length - 1].nested = true;
      stack.push({ prelude: css.slice(last, i).trim(), start: i + 1, nested: false });
      last = i + 1;
    } else if (css[i] === "}") {
      const open = stack.pop()!;
      if (!open.nested) out.push({ prelude: open.prelude, body: css.slice(open.start, i).trim() });
      last = i + 1;
    } else if (css[i] === ";") last = i + 1;
  }
  return out;
}

const list = (text: string) => text.split(",").map((s) => s.trim()).filter(Boolean);
const classes = (selector: string) => [...selector.matchAll(/\.([\w-]+)/g)].map((m) => m[1]);

const base = sheets.find((s) => s.file === "base.css")!;
const rule = rules(base.css).find((r) => r.prelude.includes(":has(:is(.menu"))!;
// The subject is the scroll container itself: whitespace before its :not() would
// be a descendant combinator and hide the wrong elements' scrollbars.
const shape = /^:root\[data-platform="linux"\]:has\(:is\(([^)]*)\)\) :is\(([^)]*)\):not\( ?:is\(([^)]*)\) \*\)$/;
const [, opened, scrollers, exempt] = rule.prelude.replace(/\s+/g, " ").match(shape) ?? [];
const popups = list(opened ?? "");
const popupClasses = new Set(popups.flatMap(classes));
const scrollerClasses = new Set(list(scrollers ?? "").flatMap(classes));

describe("popup scrollbar rule", () => {
  it("hides only the scrollbars outside an open popup, only on Linux, and changes nothing else", () => {
    expect(rule.prelude.replace(/\s+/g, " ")).toMatch(shape);
    expect(list(exempt)).toEqual(popups);
    expect(rule.body.replace(/;$/, "")).toBe("scrollbar-width: none");
    // The subject is a class list, so opening a popup restyles only those elements.
    expect(list(scrollers).every((s) => /^\.[\w-]+$/.test(s))).toBe(true);
  });

  it("names every scroll container in the stylesheets", () => {
    const missing: string[] = [];
    for (const { file, css } of sheets) {
      for (const { prelude, body } of rules(css)) {
        if (!/(^|[;\s])overflow(-x|-y|-block|-inline)?\s*:[^;]*\b(auto|scroll)\b/.test(body)) continue;
        for (const selector of list(prelude)) {
          const subject = classes(selector.split(/[\s>+~]+/).pop()!);
          if (!subject.some((c) => scrollerClasses.has(c) || popupClasses.has(c))) missing.push(`${file}: ${selector}`);
        }
      }
    }
    expect(scrollerClasses.size).toBeGreaterThan(40);
    expect(missing).toEqual([]);
  });

  it("names every menu and popover root in the components", () => {
    const missing: string[] = [];
    let seen = 0;
    for (const file of fs.readdirSync(src, { recursive: true })) {
      if (!/\.tsx$/.test(file)) continue;
      const text = fs.readFileSync(new URL(file, src), "utf8");
      // Floating roots: menus, popover-API elements and non-<dialog> dialogs (a <dialog> is modal or the model popover).
      for (const match of text.matchAll(/<(?!dialog\b)[a-z]+\b(?:[^>"{]|"[^"]*"|\{(?:[^{}]|\{[^{}]*\})*\})*?\b(?:role="(?:menu|dialog)"|popover=)/g)) {
        seen++;
        const tag = match[0];
        const own = [...tag.matchAll(/class=(?:"([^"]*)"|\{`([^`]*)`\})/g)].flatMap((m) => (m[1] ?? m[2]).split(/\s+/));
        // A dialog directly inside a listed popup (Go to's panel in its scrim) is covered by that popup.
        const before = text.slice(Math.max(0, match.index - 200), match.index);
        const inside = [...before.matchAll(/class="([^"]*)"/g)].some((m) => m[1].split(/\s+/).some((c) => popupClasses.has(c)));
        const popover = /\bpopover=/.test(tag) && popups.includes(":popover-open");
        if (!own.some((c) => popupClasses.has(c)) && !inside && !popover) missing.push(`${file}: ${tag.slice(0, 80)}`);
      }
    }
    expect(seen).toBeGreaterThan(8);
    expect(missing).toEqual([]);
  });
});
