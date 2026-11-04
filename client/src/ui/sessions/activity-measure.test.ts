import { expect, it } from "vitest";
import { Virtualizer } from "@tanstack/solid-virtual";
import { measureActivityRows } from "./activity-virtual";

it("finishes retained row measurements while initial placement still counts as scrolling", () => {
  let writes = 0;
  const virtualizer = new Virtualizer<HTMLElement, HTMLElement>({
    count: 3, getScrollElement: () => null, estimateSize: () => 96, gap: 10,
    initialRect: { width: 400, height: 300 },
    observeElementRect: () => undefined, observeElementOffset: () => undefined,
    measureElement: row => row.getBoundingClientRect().height,
    scrollToFn: () => { writes++; },
  });
  virtualizer.getVirtualItems();
  virtualizer.isScrolling = true;
  virtualizer.shouldAdjustScrollPositionOnItemSizeChange = () => false;
  const rows = [150, 1200, 70].map((height, index) => ({
    dataset: { index: String(index) }, isConnected: true,
    getAttribute: () => String(index), getBoundingClientRect: () => ({ height }),
  } as unknown as HTMLElement));
  rows.forEach(row => virtualizer.measureElement(row));
  expect(virtualizer.itemSizeCache.size).toBe(0);
  measureActivityRows(rows, virtualizer.resizeItem);
  // These nodes can now unmount: later windows use the actual row positions,
  // instead of changing their total range from three 96 px estimates.
  expect(virtualizer.getTotalSize()).toBe(1440);
  expect(virtualizer.measurementsCache.map(row => row.start)).toEqual([0, 160, 1370]);
  expect(virtualizer.itemSizeCache.size).toBe(3);
  expect(writes).toBe(0);
});
