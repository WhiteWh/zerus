import { describe, expect, it } from "vitest";
import { activityFlow, activityGeometryValid, activityRange, activityText, checkActivityGeometry, type ActivityGeometryRow } from "./activity-virtual";
import { loadMarkdown } from "./markdown";
import type { TranscriptItem } from "./types";

const geometryRow = (index: number, top: number, bottom: number, start = top): ActivityGeometryRow => ({
  index, key: `item-${index}`, top, bottom, height: bottom - top, start, domStart: top,
});

describe("Activity copy and virtual retention", () => {
  it("copies every loaded item as visible text, including only expanded disclosures", async () => {
    await loadMarkdown();
    const items: TranscriptItem[] = [
      { id: "first", kind: "agent", at: "10:00", body: "First **reply** with `code` and https://example.com." },
      { id: "tool", kind: "tools", at: "10:01", title: "Bash", body: "npm test", lines: ["hidden output"] },
      { id: "media", kind: "media", media: "voice", at: "10:02", state: "submitted", durationMs: 60_000,
        waveform: [], transcript: "voice transcript" },
      { id: "last", kind: "user", at: "10:03", body: "Last loaded message" },
    ];
    const collapsed = activityText(items);
    expect(collapsed).toContain("First reply with code and https://example.com.");
    expect(collapsed).toContain("Last loaded message");
    expect(collapsed).not.toContain("**reply**");
    expect(collapsed).not.toContain("hidden output");
    expect(collapsed).not.toContain("voice transcript");
    expect(collapsed.indexOf("First reply")).toBeLessThan(collapsed.indexOf("Last loaded message"));
    const expanded = activityText(items, { tool: true, media: true });
    expect(expanded).toContain("hidden output");
    expect(expanded).toContain("voice transcript");
  });

  it("keeps a loading or playing media row mounted while it is scrolled out", () => {
    expect(activityRange({ startIndex: 6, endIndex: 7, overscan: 1, count: 12 }, 1)).toEqual([1, 5, 6, 7, 8]);
  });

  it("copies the current CommonMark tree without losing nested text or table cells", async () => {
    await loadMarkdown();
    const text = activityText([{ id: "nested", kind: "agent", at: "10:00", body:
      "# Heading\n\n> A **quoted** ~~reply~~.\n\n- First\n  - Nested `code`\n\n| Name | Value |\n| --- | --- |\n| One | **Two** |" }]);
    expect(text).toContain("Heading");
    expect(text).toContain("A quoted reply.");
    expect(text).toContain("Nested code");
    expect(text).toContain("Name\tValue\nOne\tTwo");
    expect(text).not.toContain("**");
    expect(text).not.toContain("~~");
  });

  it("retains the playing row in order without duplicating visible rows", () => {
    const range = { startIndex: 4, endIndex: 5, overscan: 1, count: 10 };
    expect(activityRange(range, -1)).toEqual([3, 4, 5, 6]);
    expect(activityRange(range, 0)).toEqual([0, 3, 4, 5, 6]);
    expect(activityRange(range, 8)).toEqual([3, 4, 5, 6, 8]);
    expect(activityRange(range, 5)).toEqual([3, 4, 5, 6]);
    expect(activityRange(range, 10)).toEqual([3, 4, 5, 6]);
    expect(activityRange(range, 0, [1, 2, 9, 9, -1, 10])).toEqual([0, 1, 2, 3, 4, 5, 6, 9]);
  });
});

describe("Activity virtual geometry", () => {
  it("accepts disjoint mounted rows with a visible first row", () => {
    const report = checkActivityGeometry([geometryRow(4, 100, 220, 0), geometryRow(5, 230, 410, 130)], 100, 500);
    expect(report.overlaps).toEqual([]);
    expect(report.firstVisibleGap).toBe(0);
    expect(activityGeometryValid(report)).toBe(true);
  });

  it("reports painted and virtual-start overlap for the same adjacent rows", () => {
    const report = checkActivityGeometry([geometryRow(4, 100, 320, 0), geometryRow(5, 300, 350, 200)], 100, 500);
    expect(report.overlaps).toHaveLength(1);
    expect(report.overlaps[0]).toMatchObject({ pixels: 20, virtualPixels: 20 });
    expect(activityGeometryValid(report)).toBe(false);
  });

  it("rejects a first-visible gap over the 96 px estimate and empty samples", () => {
    const gap = checkActivityGeometry([geometryRow(8, 220, 350, 120)], 100, 500);
    expect(gap.firstVisibleGap).toBe(120);
    expect(activityGeometryValid(gap)).toBe(false);
    expect(activityGeometryValid(checkActivityGeometry([], 100, 500))).toBe(false);
    expect(activityGeometryValid(checkActivityGeometry([geometryRow(8, -300, -200)], 100, 500))).toBe(false);
  });

  it("places flow rows by their first start and the space before each later row", () => {
    const flow = activityFlow([
      { key: "first", start: 500, end: 650 },
      { key: "second", start: 660, end: 900 },
      { key: "retained", start: 1400, end: 1500 },
    ]);
    expect(flow.padding).toBe(500);
    expect([...flow.margins.entries()]).toEqual([["first", 0], ["second", 10], ["retained", 500]]);
    expect(activityFlow([])).toEqual({ padding: 0, margins: new Map(), starts: new Map() });
  });
});
