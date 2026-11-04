import { createSignal } from "solid-js";
import type { Lexer as MarkedLexer, Token, Tokens } from "marked";

// Agent replies follow CommonMark plus the GFM tables, strikethrough and task
// lists agents write (Qt rendered them through a CommonMark parser). The
// `marked` lexer parses; this module turns its tokens into a closed, safe tree:
// raw HTML stays literal text, links keep the Activity's external and file
// rules, nothing loads a remote image, and tables, nesting and source size are
// bounded. The lexer is a lazy chunk, preloaded at startup; Activity places its
// rows only once it is ready, so a message never changes height after placement.

export type Inline =
  | { kind: "text" | "code"; text: string }
  | { kind: "br" }
  | { kind: "bold" | "em" | "del"; children: Inline[] }
  | { kind: "external" | "file"; href: string; children: Inline[] };

export type Alignment = "left" | "center" | "right";
export interface ListItem { task: boolean; checked: boolean; blocks: MarkdownBlock[] }
export type MarkdownBlock =
  | { kind: "p" | "text"; inline: Inline[] }
  | { kind: "heading"; level: 1 | 2 | 3 | 4 | 5 | 6; inline: Inline[] }
  | { kind: "code"; text: string }
  | { kind: "quote"; blocks: MarkdownBlock[] }
  | { kind: "list"; ordered: boolean; start: number; items: ListItem[] }
  | { kind: "table"; header: Inline[][]; align: Alignment[]; rows: Inline[][][] }
  | { kind: "hr" };

/** Larger sources show as literal text: parsing them would only cost time. */
export const MAX_SOURCE = 200_000;
/** Deeper quotes and lists show their source; CommonMark itself has no limit. */
export const MAX_DEPTH = 16;
const MAX_TABLE_COLUMNS = 64;
const MAX_TABLE_CELLS = 4096;

/** The Activity's link rule: HTTP(S) opens outside, a path or file: URL is a file reference. */
export function linkKind(href: string): "external" | "file" | undefined {
  if (/^https?:\/\//i.test(href)) return "external";
  if (!/^[a-z][a-z0-9+.-]*:/i.test(href) || /^file:/i.test(href) || /^[a-z]:[\\/]/i.test(href)) return "file";
  return undefined;
}

const NAMED: Record<string, string> = {
  amp: "&", lt: "<", gt: ">", quot: "\"", apos: "'", nbsp: " ", copy: "©", reg: "®",
  hellip: "…", mdash: "—", ndash: "–", laquo: "«", raquo: "»", times: "×",
};

/** Text tokens keep entity references; the DOM receives the characters. */
export function decodeEntities(text: string): string {
  return text.replace(/&(#[xX][0-9a-fA-F]{1,6}|#[0-9]{1,7}|[a-zA-Z]{2,8});/g, (whole, name: string) => {
    if (name[0] !== "#") return NAMED[name] ?? whole;
    const code = name[1] === "x" || name[1] === "X" ? parseInt(name.slice(2), 16) : parseInt(name.slice(1), 10);
    return code > 0 && code <= 0x10ffff && !(code >= 0xd800 && code <= 0xdfff) ? String.fromCodePoint(code) : "�";
  });
}

function pushText(out: Inline[], text: string) {
  if (!text) return;
  const last = out.at(-1);
  if (last?.kind === "text") last.text += text;
  else out.push({ kind: "text", text });
}

function inlines(tokens: Token[] | undefined, out: Inline[] = []): Inline[] {
  for (const token of tokens ?? []) {
    switch (token.type) {
      case "text":
        if ("tokens" in token && token.tokens?.length) inlines(token.tokens, out);
        else pushText(out, decodeEntities(token.text));
        break;
      case "escape": pushText(out, token.text); break;
      case "codespan": out.push({ kind: "code", text: decodeEntities(token.text) }); break;
      case "br": out.push({ kind: "br" }); break;
      case "strong": out.push({ kind: "bold", children: inlines(token.tokens) }); break;
      case "em": out.push({ kind: "em", children: inlines(token.tokens) }); break;
      case "del": out.push({ kind: "del", children: inlines(token.tokens) }); break;
      case "link":
      case "image": {
        // An image is only a link to its target: nothing is fetched to show it.
        const link = token as Tokens.Link | Tokens.Image;
        const kind = linkKind(link.href);
        const children = token.type === "image" ? [{ kind: "text" as const, text: decodeEntities(link.text || link.href) }] : inlines(link.tokens);
        if (kind) out.push({ kind, href: link.href, children });
        else pushText(out, link.raw);
        break;
      }
      default: pushText(out, "raw" in token ? token.raw : "");
    }
  }
  return out;
}

const literal = (raw: string): MarkdownBlock => ({ kind: "p", inline: [{ kind: "text", text: raw.trim() }] });

function blocks(tokens: Token[], depth: number): MarkdownBlock[] {
  const out: MarkdownBlock[] = [];
  for (const token of tokens) {
    switch (token.type) {
      case "space": case "def": case "checkbox": break;
      case "paragraph": out.push({ kind: "p", inline: inlines(token.tokens) }); break;
      // A tight list item's text: inline content without a paragraph's spacing.
      case "text": out.push({ kind: "text", inline: "tokens" in token && token.tokens ? inlines(token.tokens) : [{ kind: "text", text: decodeEntities(token.text) }] }); break;
      case "heading": out.push({ kind: "heading", level: Math.min(6, Math.max(1, token.depth)) as 1, inline: inlines(token.tokens) }); break;
      case "code": out.push({ kind: "code", text: token.text }); break;
      case "hr": out.push({ kind: "hr" }); break;
      case "blockquote":
        out.push(depth >= MAX_DEPTH ? literal(token.raw) : { kind: "quote", blocks: blocks(token.tokens ?? [], depth + 1) });
        break;
      case "list": {
        const list = token as Tokens.List;
        if (depth >= MAX_DEPTH) { out.push(literal(list.raw)); break; }
        out.push({ kind: "list", ordered: list.ordered, start: typeof list.start === "number" ? list.start : 1,
          items: list.items.map((item) => ({ task: !!item.task, checked: !!item.checked, blocks: blocks(item.tokens ?? [], depth + 1) })) });
        break;
      }
      case "table": {
        const table = token as Tokens.Table;
        const columns = table.header.length;
        // Ragged rows must not amplify a short input into many empty cells.
        if (!columns || columns > MAX_TABLE_COLUMNS || (table.rows.length + 1) * columns > MAX_TABLE_CELLS) { out.push(literal(table.raw)); break; }
        out.push({ kind: "table", header: table.header.map((cell) => inlines(cell.tokens)),
          align: table.align.map((align) => align ?? "left"), rows: table.rows.map((row) => row.map((cell) => inlines(cell.tokens))) });
        break;
      }
      // Raw HTML and anything unknown keep their source as plain text.
      default: if ("raw" in token && token.raw.trim()) out.push(literal(token.raw));
    }
  }
  return out;
}

/** Without the lexer (still loading, or failed): plain paragraphs, never markup. */
export function plainBlocks(text: string): MarkdownBlock[] {
  return text.replace(/\r\n?/g, "\n").split(/\n{2,}/).filter((part) => part.trim())
    .map((part) => ({ kind: "p", inline: [{ kind: "text", text: part }] }));
}

/** The pure parse, for tests and for the loaded lexer. */
export function parseMarkdown(text: string, lexer: typeof MarkedLexer): MarkdownBlock[] {
  if (text.length > MAX_SOURCE) return plainBlocks(text);
  return blocks(lexer.lex(text, { gfm: true, breaks: false }), 0);
}

let lexer: typeof MarkedLexer | undefined;
let loading: Promise<void> | undefined;
const [ready, setReady] = createSignal(false);
/** True once the lexer loaded, or failed to (then messages show as plain text). */
export const markdownReady = ready;

export function loadMarkdown(): Promise<void> {
  return loading ??= import("marked").then(
    (module) => { lexer = module.Lexer; },
    (error) => { console.warn("Zerus Next: the Markdown parser did not load", error); },
  ).finally(() => setReady(true));
}

const cache = new Map<string, MarkdownBlock[]>();
const CACHE_SIZE = 256;

/** Parsed once per text; Activity rerenders reuse the tree. */
export function markdownTree(text: string): MarkdownBlock[] {
  if (!lexer) return plainBlocks(text);
  const hit = cache.get(text);
  if (hit) { cache.delete(text); cache.set(text, hit); return hit; }
  const tree = parseMarkdown(text, lexer);
  cache.set(text, tree);
  if (cache.size > CACHE_SIZE) cache.delete(cache.keys().next().value!);
  return tree;
}
