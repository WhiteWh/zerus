import { describe, expect, it } from "vitest";
import shownFixture from "../fixtures/new-session-shown-v1.json";
import cardFixture from "../fixtures/question-card-v1.json";
import type { CardView, LaunchShown } from "./types";
import { renderText, type Text } from "./text";
import catalog from "./i18n.ru.json";
import { translateWithCatalog } from "./i18n";

const ru = catalog as Record<string, string | string[]>;
const textIn = (language: "en" | "ru", text: Text) => renderText(text, (source, ...args) => translateWithCatalog(ru, language, source, ...args));

// The core's New session wire shape (core tests/launch_text_contract.rs writes
// the fixture). FIELDS must list exactly LaunchShown's keys (`satisfies`), and
// each fixture row must match it, so a shape change on either side fails here.
type Kind = "boolean" | "string" | "text" | "text?";
const FIELDS = {
  start_enabled: "boolean", start_text: "text", preview: "text?", outside_project: "boolean", name_problem: "text?",
  notice: "text?", automatic: "boolean", auto_enabled: "boolean", none_enabled: "boolean", worktree_target: "string",
  worktree_tooltip: "text?", choose_existing_enabled: "boolean", create_manually_enabled: "boolean",
  worktree_options_enabled: "boolean", fields_enabled: "boolean", browse_enabled: "boolean", cancel_enabled: "boolean",
  open_terminal_enabled: "boolean", open_terminal_tooltip: "text", clone_launch_due: "boolean",
} as const satisfies Record<keyof LaunchShown, Kind>;

function isText(value: unknown): value is Text {
  if (!value || typeof value !== "object") return false;
  const { source, args, ...rest } = value as Record<string, unknown>;
  return typeof source === "string" && source !== "" && Object.keys(rest).length === 0
    && (args === undefined || (Array.isArray(args) && args.every((a) => {
      const { kind, value: v } = a as { kind: string; value: unknown };
      return kind === "verbatim" ? typeof v === "string" : kind === "number" ? typeof v === "number" : kind === "text" && isText(v);
    })));
}

function texts(row: Record<string, unknown>): Text[] {
  expect(Object.keys(row).sort()).toEqual(Object.keys(FIELDS).sort());
  const out: Text[] = [];
  for (const [key, kind] of Object.entries(FIELDS) as [string, Kind][]) {
    const value = row[key];
    if (kind === "boolean") expect(typeof value, key).toBe("boolean");
    else if (kind === "string") expect(typeof value, key).toBe("string");
    else if (kind === "text" || value !== null) {
      expect(isText(value), `${key}: ${JSON.stringify(value)}`).toBe(true);
      out.push(value as Text);
    }
  }
  return out;
}

describe("core Text", () => {
  it("renders typed arguments and keeps verbatim values untranslated", () => {
    const tooltip: Text = { source: "New branch: %1\nStart from: HEAD", args: [{ kind: "verbatim", value: "work-1" }] };
    expect(textIn("en", tooltip)).toBe("New branch: work-1\nStart from: HEAD");
    expect(textIn("ru", tooltip)).toBe("Новая ветка: work-1\nОснова: HEAD");
  });

  it("the New session wire fixture matches LaunchShown and every Text has Russian", () => {
    const rows = shownFixture as unknown as Record<string, unknown>[];
    expect(rows.length).toBeGreaterThan(10);
    const all = rows.flatMap(texts);
    for (const text of all) {
      const english = textIn("en", text), russian = textIn("ru", text);
      expect(russian, text.source).not.toBe(english);
      expect(russian, text.source).not.toMatch(/%\d|%n/);
      for (const arg of text.args ?? []) if (arg.kind === "verbatim") expect(russian).toContain(arg.value);
    }
  });

  // The question card (core tests/question_card_text_contract.rs). CARD must
  // list exactly CardView's keys; every Text inside renders in Russian.
  const CARD = {
    heading: "text", progress: "text?", pending: "text?", pending_tip: "text", nav: "object?", tabs_visible: "boolean", page: "number",
    forms: "array", status: "text", status_error: "boolean", available: "boolean", locked: "boolean", review: "boolean", created_at: "number",
    submit: "object", approve: "object", deny: "object", skip: "object", previous: "object", next: "object", retry: "boolean", skip_tip: "text", retry_tip: "text",
  } as const satisfies Record<keyof CardView, string>;
  function allTexts(value: unknown, out: Text[] = []): Text[] {
    if (isText(value)) out.push(value);
    else if (Array.isArray(value)) value.forEach((v) => allTexts(v, out));
    else if (value && typeof value === "object") Object.values(value).forEach((v) => allTexts(v, out));
    return out;
  }
  it("the question card wire fixture matches CardView and its prose has Russian", () => {
    const cards = cardFixture as unknown as Record<string, unknown>[];
    expect(cards.length).toBeGreaterThan(5);
    for (const view of cards) {
      expect(Object.keys(view).sort()).toEqual(Object.keys(CARD).sort());
      for (const [key, kind] of Object.entries(CARD)) if (kind === "text" || (kind === "text?" && view[key] !== null)) expect(isText(view[key]), key).toBe(true);
    }
    const texts = cards.flatMap((view) => allTexts(view));
    expect(texts.length).toBeGreaterThan(100);
    for (const text of texts) {
      const english = textIn("en", text), russian = textIn("ru", text);
      expect(russian, text.source).not.toMatch(/%\d|%n/);
      for (const arg of text.args ?? []) if (arg.kind === "verbatim") expect(russian).toContain(arg.value);
      // Own prose translates; joins of verbatim values and empty texts stay as they are.
      if (/[A-Za-z]{2}/.test(text.source.replace(/%\d/g, ""))) expect(russian, text.source).not.toBe(english);
    }
  });
});
