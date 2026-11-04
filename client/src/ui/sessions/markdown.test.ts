import { describe, expect, it } from "vitest";
import { Lexer } from "marked";
import { MAX_DEPTH, MAX_SOURCE, decodeEntities, linkKind, loadMarkdown, markdownReady, markdownTree, parseMarkdown, plainBlocks, type Inline, type MarkdownBlock } from "./markdown";
import { SCREENSHOT_TABLE } from "../../perf/fixtures/markdownTables";

const parse = (text: string) => parseMarkdown(text, Lexer);
const t = (text: string): Inline => ({ kind: "text", text });
const p = (...inline: Inline[]): MarkdownBlock => ({ kind: "p", inline });
const item = (text: string, extra: Partial<{ task: boolean; checked: boolean }> = {}) =>
  ({ task: false, checked: false, ...extra, blocks: [{ kind: "text", inline: [t(text)] }] });

describe("Activity Markdown blocks (CommonMark)", () => {
  it("lets a list interrupt the paragraph before it, as the owner's reply showed", () => {
    expect(parse("My proposal:\n- one\n- two")).toEqual([
      p(t("My proposal:")),
      { kind: "list", ordered: false, start: 1, items: [item("one"), item("two")] },
    ]);
  });

  it("parses ordered lists with their start, nested lists and task lists", () => {
    expect(parse("3. third\n4. fourth")).toEqual([{ kind: "list", ordered: true, start: 3, items: [item("third"), item("fourth")] }]);
    const [nested] = parse("- outer\n  - inner");
    if (nested.kind !== "list") throw Error("no list");
    expect(nested.items[0].blocks).toEqual([
      { kind: "text", inline: [t("outer")] },
      { kind: "list", ordered: false, start: 1, items: [item("inner")] },
    ]);
    expect(parse("- [x] done\n- [ ] open")).toEqual([
      { kind: "list", ordered: false, start: 1, items: [item("done", { task: true, checked: true }), item("open", { task: true })] },
    ]);
  });

  it("parses ATX and setext headings, quotes, rules and code blocks", () => {
    expect(parse("# One\n\nHeading\n---\n\n> quoted *text*\n\n***\n\n    indented\n\n```ts\nconst a = 1 < 2;\n```")).toEqual([
      { kind: "heading", level: 1, inline: [t("One")] },
      { kind: "heading", level: 2, inline: [t("Heading")] },
      { kind: "quote", blocks: [p(t("quoted "), { kind: "em", children: [t("text")] })] },
      { kind: "hr" },
      { kind: "code", text: "indented" },
      { kind: "code", text: "const a = 1 < 2;" },
    ]);
  });

  it("keeps fenced tables as code, including longer and unclosed fences", () => {
    expect(parse("Before\n\n````md\n" + SCREENSHOT_TABLE + "\n```\n````\nAfter")).toEqual([
      p(t("Before")), { kind: "code", text: SCREENSHOT_TABLE + "\n```" }, p(t("After")),
    ]);
    expect(parse("~~~md\n" + SCREENSHOT_TABLE)).toEqual([{ kind: "code", text: SCREENSHOT_TABLE }]);
  });
});

describe("Activity Markdown inlines", () => {
  it("parses emphasis, strong, strikethrough, code spans, escapes and hard breaks", () => {
    expect(parse("*em* **bold** ~~gone~~ `a < b & c` \\*not em\\* line  \nnext")).toEqual([p(
      { kind: "em", children: [t("em")] }, t(" "), { kind: "bold", children: [t("bold")] }, t(" "),
      { kind: "del", children: [t("gone")] }, t(" "), { kind: "code", text: "a < b & c" }, t(" *not em* line"),
      { kind: "br" }, t("next"),
    )]);
  });

  it("keeps raw HTML literal and decodes entity references", () => {
    expect(parse("x <b>bold</b> &amp; &#169; &#x1F600; &bogus;")).toEqual([p(t("x <b>bold</b> & \u00a9 \u{1F600} &bogus;"))]);
    expect(parse("<div>\nraw\n</div>")).toEqual([p(t("<div>\nraw\n</div>"))]);
    expect(decodeEntities("&#0; &#xD800;")).toBe("\ufffd \ufffd");
  });

  it("links HTTP URLs outside and paths as file references, keeping other schemes as text", () => {
    expect(parse("Use [archive](/tmp/a.tar), https://example.com/doc. and www.example.com/docs")).toEqual([p(
      t("Use "), { kind: "file", href: "/tmp/a.tar", children: [t("archive")] }, t(", "),
      { kind: "external", href: "https://example.com/doc", children: [t("https://example.com/doc")] }, t(". and "),
      { kind: "external", href: "http://www.example.com/docs", children: [t("www.example.com/docs")] },
    )]);
    expect(parse("[custom](mailto:person@example.com) [bad](javascript:alert(1))")).toEqual([
      p(t("[custom](mailto:person@example.com) [bad](javascript:alert(1))")),
    ]);
    for (const href of ["docs/a.md:12", "file:///tmp/b.md", "C:/work/c.cpp:4", "C:\\work\\c.cpp:4"]) expect(linkKind(href)).toBe("file");
    expect(linkKind("ssh://host")).toBeUndefined();
  });

  it("never loads an image: it becomes a link to its target", () => {
    expect(parse("![chart](https://example.com/c.png) ![](/tmp/local.png)")).toEqual([p(
      { kind: "external", href: "https://example.com/c.png", children: [t("chart")] }, t(" "),
      { kind: "file", href: "/tmp/local.png", children: [t("/tmp/local.png")] },
    )]);
  });
});

describe("Activity GFM tables", () => {
  it("keeps the reported Russian table's six rows, bold cell and long last column", () => {
    const blocks = parse("Before.\n\n" + SCREENSHOT_TABLE + "\n\nAfter.");
    expect(blocks[0]).toEqual(p(t("Before.")));
    const table = blocks[1];
    if (table.kind !== "table") throw Error("The reply did not contain a table");
    expect(table.header).toEqual([[t("Пункт постановки")], [t("Фактическое состояние")]]);
    expect(table.rows).toHaveLength(6);
    expect(table.rows[0][1][0]).toEqual({ kind: "bold", children: [t("Работает в dev")] });
    expect(JSON.stringify(table.rows[5][1])).toContain("Произвольное добавление узла с полной проверкой ещё не принято");
    expect(blocks[2]).toEqual(p(t("After.")));
  });

  it("supports alignment and optional outer pipes, padding or discarding body cells", () => {
    expect(parse("Left | Middle | Right\n:--- | :-: | ---:\none | two\nthree | four | five | ignored")).toEqual([{
      kind: "table", header: [[t("Left")], [t("Middle")], [t("Right")]], align: ["left", "center", "right"],
      rows: [[[t("one")], [t("two")], []], [[t("three")], [t("four")], [t("five")]]],
    }]);
  });

  it("unescapes cell pipes, including inside code and bold", () => {
    const [table] = parse(["| f\\|oo | Value |", "| --- | --- |", "| b `\\|` az | **\\|** |"].join("\n"));
    if (table.kind !== "table") throw Error("Missing escaped-pipe table");
    expect(table.header[0]).toEqual([t("f|oo")]);
    expect(table.rows[0][0]).toEqual([t("b "), { kind: "code", text: "|" }, t(" az")]);
    expect(table.rows[0][1]).toEqual([{ kind: "bold", children: [t("|")] }]);
  });

  it("rejects mismatched delimiters and ordinary pipe prose, and admits header-only tables", () => {
    expect(parse("| one | two |\n| --- |\n| body |")).toEqual([p(t("| one | two |\n| --- |\n| body |"))]);
    expect(parse("A | B\nwords | more")).toEqual([p(t("A | B\nwords | more"))]);
    expect(parse("| A |\n| --- |\n\n- list")[0]).toEqual({ kind: "table", header: [[t("A")]], align: ["left"], rows: [] });
  });

  it("keeps oversized ragged tables literal instead of amplifying empty cells", () => {
    const header = Array(64).fill("column").join(" | "), delimiter = Array(64).fill("---").join(" | ");
    const text = [header, delimiter, ...Array(70).fill("one cell")].join("\n");
    expect(parse(text)).toEqual([p(t(text))]);
    const wide = [Array(65).fill("c").join(" | "), Array(65).fill("-").join(" | ")].join("\n");
    expect(parse(wide)).toEqual([p(t(wide))]);
  });
});

describe("Activity Markdown bounds and loading", () => {
  it("shows nesting beyond the depth limit as its source", () => {
    const deep = ">".repeat(MAX_DEPTH + 4) + " deep";
    let block = parse(deep)[0];
    let depth = 0;
    while (block.kind === "quote") { block = block.blocks[0]; depth++; }
    expect(depth).toBe(MAX_DEPTH);
    expect(block).toEqual(p(t(">".repeat(4) + " deep")));
  });

  it("keeps oversized sources as plain paragraphs without parsing", () => {
    const huge = "**x** ".repeat(Math.ceil(MAX_SOURCE / 6) + 10);
    expect(parse(huge)).toEqual(plainBlocks(huge));
    expect(plainBlocks("a\n\n\nb\r\n")).toEqual([p(t("a")), p(t("b\n"))]);
  });

  it("renders plain text until the lazily loaded parser is ready, then CommonMark", async () => {
    if (!markdownReady()) expect(markdownTree("- one")).toEqual([p(t("- one"))]);
    await loadMarkdown();
    expect(markdownReady()).toBe(true);
    expect(markdownTree("- one")).toEqual([{ kind: "list", ordered: false, start: 1, items: [item("one")] }]);
    expect(markdownTree("- one")).toBe(markdownTree("- one"));
  });
});
