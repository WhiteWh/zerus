// Skin format v1: a declarative, stack-agnostic JSON document.
//
// L0  tokens      colors, fonts, radii, density, motion, shadows
// L1  components  named variants of existing components (enum values only)
// L2  layout      named layout variants of existing screens (enum values only)
//
// A skin never carries CSS, selectors, URLs or code. The base skin
// (zerus-dark) defines the complete token set; other skins may only override
// tokens that exist there. Values are validated per token type and compiled to
// CSS custom properties; other stacks (Qt, Compose, SwiftUI) can compile the
// same JSON with their own generators.

export type Appearance = "dark" | "light";

export interface SkinFile {
  schema: 1;
  id: string;
  name: string;
  appearance: Appearance;
  extends?: string;
  tokens?: Record<string, Record<string, unknown>>;
  components?: Partial<ComponentVariants>;
  layout?: Partial<LayoutVariants>;
}

export const COMPONENT_VARIANTS = {
  "status-badge": ["pill", "dot"],
  "agent-mark": ["chip", "glyph"],
  "tree-lines": ["lines", "indent"],
  "attention-edge": ["bar", "tint"],
} as const;

export const LAYOUT_VARIANTS = {
  rail: ["icons", "labeled"],
  "sessions-pane": ["left", "right"],
  inspector: ["docked", "collapsed"],
  "activity-column": ["centered", "full"],
} as const;

export type ComponentVariants = { -readonly [K in keyof typeof COMPONENT_VARIANTS]: (typeof COMPONENT_VARIANTS)[K][number] };
export type LayoutVariants = { -readonly [K in keyof typeof LAYOUT_VARIANTS]: (typeof LAYOUT_VARIANTS)[K][number] };

export interface ResolvedSkin {
  id: string;
  name: string;
  appearance: Appearance;
  tokens: Record<string, Record<string, unknown>>;
  components: ComponentVariants;
  layout: LayoutVariants;
}

type TokenType = "color" | "colors" | "percent" | "px" | "ratio" | "density" | "ms" | "easing" | "fonts" | "shadow";

function tokenType(group: string, key: string): TokenType | undefined {
  switch (group) {
    case "color":
    case "agent":
    case "list":
      return "color";
    case "entity":
      return key === "palette" ? "colors" : "percent";
    case "font":
      return key === "base" ? "px" : key === "line" ? "ratio" : "fonts";
    case "radius":
      return "px";
    case "density":
      return "density";
    case "motion":
      return key === "ease" ? "easing" : "ms";
    case "shadow":
      return "shadow";
  }
  return undefined;
}

const KEYWORD = /^transparent$/;
const HEX = /^#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const RGB = /^rgba?\(\s*\d{1,3}\s*,\s*\d{1,3}\s*,\s*\d{1,3}\s*(?:,\s*(?:0|1|0?\.\d+)\s*)?\)$/i;
const REF = /^\{([a-z]+)\.([a-z0-9-]+)\}$/;
const FAMILY = /^[A-Za-z0-9 -]{1,48}$/;
const EASING = /^(?:linear|ease|ease-in|ease-out|ease-in-out|cubic-bezier\(\s*-?[\d.]+\s*,\s*-?[\d.]+\s*,\s*-?[\d.]+\s*,\s*-?[\d.]+\s*\))$/;
const SHADOW_PART = /^(?:inset\s+)?(?:-?\d+(?:\.\d+)?(?:px)?\s+){2,4}rgba?\([\d\s.,]+\)$/;

export class SkinError extends Error {}

function validRgb(value: string): boolean {
  if (!RGB.test(value)) return false;
  const parts = value.slice(value.indexOf("(") + 1, -1).split(",").map(Number);
  return parts.length === (value.toLowerCase().startsWith("rgba") ? 4 : 3) && parts.every((n, i) => Number.isFinite(n) && n >= 0 && n <= (i === 3 ? 1 : 255));
}

function isColor(value: unknown): boolean {
  if (typeof value !== "string") return false;
  const ref = REF.exec(value);
  return HEX.test(value) || validRgb(value) || !!ref && ["color", "agent", "list"].includes(ref[1]) || KEYWORD.test(value);
}

function check(type: TokenType, path: string, value: unknown): void {
  const fail = (why: string) => {
    throw new SkinError(`${path}: ${why}`);
  };
  const num = (min: number, max: number) => {
    if (typeof value !== "number" || !Number.isFinite(value) || value < min || value > max) fail(`expected a number in ${min}..${max}`);
  };
  switch (type) {
    case "color":
      if (!isColor(value)) fail("expected #hex, rgb()/rgba() or a {group.token} reference");
      return;
    case "colors":
      if (!Array.isArray(value) || value.length < 1 || value.length > 16 || !value.every((c) => typeof c === "string" && (HEX.test(c) || validRgb(c))))
        fail("expected 1..16 literal colors");
      return;
    case "percent":
      return num(0, 100);
    case "px":
      return num(0, 32);
    case "ratio":
      return num(1, 2);
    case "density":
      return num(0.8, 1.3);
    case "ms":
      return num(0, 5000);
    case "easing":
      if (typeof value !== "string" || !EASING.test(value)) fail("expected a CSS easing keyword or cubic-bezier()");
      if (typeof value === "string" && value.startsWith("cubic-bezier")) {
        const p = value.slice(value.indexOf("(") + 1, -1).split(",").map(Number);
        if (p.length !== 4 || !p.every((n, i) => Number.isFinite(n) && (i % 2 === 1 || n >= 0 && n <= 1))) fail("invalid cubic-bezier() coordinates");
      }
      return;
    case "fonts":
      if (!Array.isArray(value) || value.length < 1 || value.length > 8 || !value.every((f) => typeof f === "string" && FAMILY.test(f)))
        fail("expected 1..8 font family names");
      return;
    case "shadow":
      if (typeof value !== "string" || value.length > 160 || !value.split(/,(?![^(]*\))/).every((p) => SHADOW_PART.test(p.trim()) && validRgb(p.slice(p.search(/rgba?/i)).trim())))
        fail("expected box-shadow lengths with an rgb()/rgba() color");
      return;
  }
}

function validateFile(file: SkinFile, base: SkinFile | undefined): void {
  if (file.schema !== 1) throw new SkinError(`${file.id}: unsupported schema ${String(file.schema)}`);
  if (!/^[a-z0-9-]{1,40}$/.test(file.id)) throw new SkinError("id: lowercase letters, digits and dashes");
  if (file.appearance !== "dark" && file.appearance !== "light") throw new SkinError(`${file.id}: appearance must be dark or light`);
  for (const [group, values] of Object.entries(file.tokens ?? {})) {
    if (base && !Object.hasOwn(base.tokens ?? {}, group)) throw new SkinError(`${file.id}: unknown token group ${group}`);
    for (const [key, value] of Object.entries(values)) {
      const type = tokenType(group, key);
      if (!type) throw new SkinError(`${file.id}: unknown token group ${group}`);
      if (base && !Object.hasOwn((base.tokens ?? {})[group] ?? {}, key)) throw new SkinError(`${file.id}: unknown token ${group}.${key}`);
      check(type, `${file.id}: ${group}.${key}`, value);
    }
  }
  for (const [name, value] of Object.entries(file.components ?? {})) {
    const allowed = (COMPONENT_VARIANTS as Record<string, readonly string[]>)[name];
    if (!Array.isArray(allowed) || typeof value !== "string" || !allowed.includes(value)) throw new SkinError(`${file.id}: components.${name} must be one of ${allowed?.join(", ") ?? "(unknown)"}`);
  }
  for (const [name, value] of Object.entries(file.layout ?? {})) {
    const allowed = (LAYOUT_VARIANTS as Record<string, readonly string[]>)[name];
    if (!Array.isArray(allowed) || typeof value !== "string" || !allowed.includes(value)) throw new SkinError(`${file.id}: layout.${name} must be one of ${allowed?.join(", ") ?? "(unknown)"}`);
  }
}

/** Validate and resolve `extends` chains. The first file is the base skin. */
export function resolveSkins(files: SkinFile[], ids?: readonly string[]): ResolvedSkin[] {
  const byId = new Map(files.map((f) => [f.id, f]));
  const base = files[0];
  validateFile(base, undefined);
  const resolve = (file: SkinFile, seen: Set<string>): ResolvedSkin => {
    if (seen.has(file.id)) throw new SkinError(`${file.id}: extends cycle`);
    seen.add(file.id);
    if (file !== base) validateFile(file, base);
    const parent = file.extends ? byId.get(file.extends) : file === base ? undefined : base;
    if (file.extends && !parent) throw new SkinError(`${file.id}: extends unknown skin ${file.extends}`);
    const inherited = parent ? resolve(parent, seen) : undefined;
    const tokens: Record<string, Record<string, unknown>> = {};
    for (const source of [inherited?.tokens ?? {}, file.tokens ?? {}])
      for (const [group, values] of Object.entries(source)) tokens[group] = { ...(tokens[group] ?? {}), ...values };
    return {
      id: file.id,
      name: file.name,
      appearance: file.appearance,
      tokens,
      components: { ...(inherited?.components ?? {}), ...(file.components ?? {}) } as ComponentVariants,
      layout: { ...(inherited?.layout ?? {}), ...(file.layout ?? {}) } as LayoutVariants,
    };
  };
  return files.filter(f => !ids || ids.includes(f.id)).map((f) => {
    const result = resolve(f, new Set());
    const checkRef = (group: string, key: string, seen: Set<string>): void => {
      const token = `${group}.${key}`;
      if (seen.has(token)) throw new SkinError(`${f.id}: token reference cycle at ${token}`);
      const value = result.tokens[group]?.[key];
      if (value === undefined) throw new SkinError(`${f.id}: unknown token reference ${token}`);
      const ref = typeof value === "string" ? REF.exec(value) : undefined;
      if (ref) { seen.add(token); checkRef(ref[1], ref[2], seen); }
    };
    for (const [group, values] of Object.entries(result.tokens)) for (const key of Object.keys(values)) checkRef(group, key, new Set());
    return result;
  });
}

const varName = (group: string, key: string) => `--z-${group}-${key}`;

function cssValue(type: TokenType, value: unknown): string {
  switch (type) {
    case "color": {
      const ref = REF.exec(String(value));
      return ref ? `var(${varName(ref[1], ref[2])})` : String(value);
    }
    case "percent":
      return `${value}%`;
    case "px":
      return `${value}px`;
    case "ms":
      return `${value}ms`;
    case "fonts":
      return (value as string[]).map((f) => (/^[a-z-]+$/.test(f) ? f : `"${f}"`)).join(", ");
    default:
      return String(value);
  }
}

/** Compile one resolved skin to a single CSS rule of custom properties. */
export function compileSkin(skin: ResolvedSkin): string {
  const lines: string[] = [`color-scheme: ${skin.appearance};`];
  for (const [group, values] of Object.entries(skin.tokens)) {
    for (const [key, value] of Object.entries(values)) {
      const type = tokenType(group, key)!;
      if (type === "colors") (value as string[]).forEach((c, i) => lines.push(`${varName(group, key)}-${i}: ${c};`));
      else lines.push(`${varName(group, key)}: ${cssValue(type, value)};`);
    }
  }
  return `:root[data-skin="${skin.id}"] {\n  ${lines.join("\n  ")}\n}`;
}
