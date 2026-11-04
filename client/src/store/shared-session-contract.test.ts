import { beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } });
});
vi.mock("../bridge", () => ({
  onCreations: vi.fn(async () => () => {}), creationsView: vi.fn(async () => []),
  archivePage: vi.fn(), resetArchivePages: vi.fn(async () => {}), cancelArchivePages: vi.fn(async () => {}),
  runAction: vi.fn(), inspectSession: vi.fn(), pendingReadMarks: vi.fn(async () => []),
  attentionMarks: vi.fn(async () => ({ marks: {}, replies: {} })), updateAttentionMarks: vi.fn(async () => {}),
  queueReadMarks: vi.fn(async () => []), sessionReference: vi.fn(), copyCommand: vi.fn(), copyText: vi.fn(), setNotifications: vi.fn(),
}));

import * as bridge from "../bridge";
import { applyBatch, catalog, computerFresh, setDraftKeys, toSession } from "./catalog";
import { attentionKey, marks } from "./marks";
import { needsAttention } from "./attention";
import { archive } from "./archive";
import { applyGrouping, emptyGrouping } from "./organization";
import { matchesFilter, setAccountFilter, setMachineFilter, setProjectFilter, setReadSelectionKey, setSearch, setSelectedKey, setStateFilter, type StateFilter } from "./ui";
import { listRows } from "../ui/sessions/model";
import { optionalQuestionCount, optionalQuestionCountText } from "../ui/sessions/optional-questions";
import { emptyBatch, type ArchiveRow, type ViewRow } from "../types";
import filters from "../../../mobile/android/app/src/test/resources/session-filter-contract.json";
import questions from "../../../mobile/android/app/src/test/resources/optional-question-contract.json";
import projection from "../../core/tests/fixtures/session-contract-view.json";

// Rust's session_contract.rs verifies these are actual serialized ViewRow fields
// from the canonical raw rows, including CatalogState's four retained sequences.
const wire = projection as unknown as { filters: Record<string, ViewRow>; questions: Record<string, ViewRow>; sequences: Record<string, ViewRow[]> };
const COMPUTER = "contract";
const KEY = "run:contract";
const ID = `${COMPUTER}\n${KEY}`;
const flush = async () => { for (let n = 0; n < 12; n++) await Promise.resolve(); };
const shown = () => listRows().filter((row) => row.type === "card").map((row) => row.session.name);
const chip = () => optionalQuestionCount(catalog.rows[ID], computerFresh(COMPUTER));

beforeEach(async () => {
  setStateFilter("all"); setSelectedKey(undefined); setReadSelectionKey(undefined);
  setMachineFilter([]); setProjectFilter([]); setAccountFilter([]); setSearch("");
  applyGrouping({ ...emptyBatch(), grouping: emptyGrouping() });
  applyBatch({ ...emptyBatch(), replaces: [{ computer: COMPUTER, rows: [] }], computers: [{ computer: COMPUTER, status: "live", attempt: 0 }] });
  setDraftKeys([]);
  vi.mocked(bridge.attentionMarks).mockResolvedValue({ marks: {}, replies: {} });
  vi.mocked(bridge.archivePage).mockResolvedValue({ rows: [], next_cursor: "", previous_cursor: null, total: 0, position: 0 });
  await marks.load();
  await flush();
});

function receive(row: ViewRow, online: boolean) {
  applyBatch({ ...emptyBatch(), computers: [{ computer: COMPUTER, status: online ? "live" : "reconnecting", attempt: online ? 0 : 1 }],
    upserts: [{ computer: COMPUTER, key: KEY, row }] });
}

it("working-one reaches the optional-question chip field", () => {
  receive(wire.questions["working-one"], true);
  const example = questions.cases.find((c) => c.name === "working-one")!;
  expect(catalog.rows[ID].optionalQuestions).toBe(example.count);
  expect(optionalQuestionCountText(chip())).toBe(example.label);
});

describe("canonical session-filter membership through the catalog, marks and list routing", () => {
  it.each(filters.cases.map((c) => [c.name, c] as const))("%s", async (name, example) => {
    const row = wire.filters[name];
    const raw = example.raw as Record<string, unknown>;
    const session = toSession(COMPUTER, KEY, row);
    vi.mocked(bridge.attentionMarks).mockResolvedValue({ marks: {
      [attentionKey(session)]: { review: raw.review_later === true, acknowledged: raw.attention_acknowledged === true ? session.fingerprint : "" },
    }, replies: {} });
    await marks.load();
    // The live catalog and paged archive are separate production sources.
    // Feeding an archive into matchesFilter("all") alone would skip that boundary.
    if (row.state === "archived") {
      vi.mocked(bridge.archivePage).mockResolvedValue({ rows: [{ ...row, archive_id: "10000000-0000-4000-8000-000000000001", archived_at: 100 } as ArchiveRow],
        next_cursor: "", previous_cursor: null, total: 1, position: 0 });
    } else {
      receive(row, example.online);
      setDraftKeys(example.draft ? [`${COMPUTER}\n${row.name}`] : []);
    }
    const matched: string[] = [];
    for (const filter of filters.filters as StateFilter[]) {
      setStateFilter(filter);
      if (filter === "archived") archive.refresh();
      await flush();
      const listed = shown().includes(row.name);
      if (listed) matched.push(filter);
      if (row.state !== "archived" && filter !== "archived") expect(listed).toBe(matchesFilter(catalog.rows[ID], filter));
    }
    expect(matched).toEqual(example.matches);
  });
});

describe("canonical optional-question chips through the serialized catalog and production selector", () => {
  it.each(questions.cases.map((c) => [c.name, c] as const))("%s", (name, example) => {
    receive(wire.questions[name], example.online);
    expect(chip()).toBe(example.count);
    expect(optionalQuestionCountText(chip())).toBe(example.label);
    if (example.raw.phase !== "input") expect(needsAttention(catalog.rows[ID], computerFresh(COMPUTER))).toBe(false);
  });

  it.each(questions.desktop_sequences.map((s) => [s.name, s] as const))("%s", (name, sequence) => {
    sequence.steps.forEach((step, n) => {
      receive(wire.sequences[name][n], step.online);
      expect(chip(), `step ${n}`).toBe(step.count);
    });
    if (name === "saved-then-unknown") expect(catalog.rows[ID].optionalQuestions).toBe(0);
    if (name === "new-conversation-unknown") expect(catalog.rows[ID].optionalQuestions).toBeUndefined();
  });

  it("hides a cached count through actual connectivity changes and restores it on reconnect", () => {
    receive(wire.questions["working-two"], true);
    const row = catalog.rows[ID];
    for (const status of ["reconnecting", "probing", "unsupported", "polled", "live", "ended", "live"]) {
      applyBatch({ ...emptyBatch(), computers: [{ computer: COMPUTER, status, attempt: 0 }] });
      expect(catalog.rows[ID]).toBe(row);
      expect(row.optionalQuestions).toBe(2);
      expect(chip()).toBe(status === "live" || status === "polled" ? 2 : 0);
    }
  });
});
