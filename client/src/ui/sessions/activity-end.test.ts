import { describe, expect, it } from "vitest";
import { createActivityEnd, newLocalSends, type ScrollGeometry } from "./activity-end";

function fixture(canWrite?: () => boolean, placed?: () => void) {
  const geometry: ScrollGeometry = { top: 0, height: 2000, viewport: 500 };
  const queued = new Map<number, () => void>();
  let serial = 0, writes = 0;
  const end = createActivityEnd(() => ({ ...geometry }), () => { writes++; geometry.top = Math.max(0, geometry.height - geometry.viewport); }, {
    request: (callback) => { const id = ++serial; queued.set(id, callback); return id; }, cancel: (id) => { queued.delete(id); },
  }, canWrite, placed);
  const frame = () => { const callbacks = [...queued.values()]; queued.clear(); callbacks.forEach((callback) => callback()); };
  return { end, geometry, queued, frame, writes: () => writes };
}

describe("Activity end settling", () => {
  it("places each selected session after render, even with the same item count", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame();
    expect(f.geometry.top).toBe(1500);
    f.end.release(); f.geometry.top = 200; expect(f.end.scrolled()).toBe(false);
    f.end.pin(); f.frame(); f.frame();
    expect(f.geometry.top).toBe(1500); expect(f.end.active()).toBe(true);
  });
  it("ignores layout scroll events before the first frame or while content grows", () => {
    const f = fixture(); f.end.pin();
    expect(f.end.scrolled()).toBe(true); f.frame(); f.frame();
    f.geometry.height += 700;
    expect(f.end.scrolled()).toBe(true); f.end.changed(); f.frame(); f.frame();
    expect(f.geometry.top).toBe(2200);
  });
  it("pins late Markdown, image and tool disclosure growth without another item", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame();
    for (const growth of [200, 600, 100]) {
      f.geometry.height += growth; f.end.changed(); f.frame(); f.frame();
      expect(f.geometry.top + f.geometry.viewport).toBe(f.geometry.height);
    }
  });
  it("settles a resize before paint, without a shifted frame", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame();
    // A question card mounts under the column, then an image row grows: each
    // resize is re-pinned in its ResizeObserver delivery, before the frame paints.
    f.geometry.viewport = 200; f.end.settle();
    expect(f.geometry.top).toBe(1800);
    f.geometry.height += 300; f.end.settle();
    expect(f.geometry.top + f.geometry.viewport).toBe(f.geometry.height);
    // The next frame only verifies: it writes nothing more at a settled end.
    const writes = f.writes(); f.frame(); f.frame();
    expect(f.writes()).toBe(writes);
  });
  it("never settles during a user scroll or after the pin is released", () => {
    let scrolling = true;
    const f = fixture(() => !scrolling); f.end.pin(); f.frame(); f.frame();
    f.geometry.height += 400; f.end.settle();
    expect(f.geometry.top).toBe(0);
    scrolling = false; f.end.release(); f.end.settle();
    expect(f.geometry.top).toBe(0); expect(f.writes()).toBe(0);
  });
  it("keeps the last row above the composer when the viewport shrinks", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame();
    f.geometry.viewport = 200; f.end.changed(); f.frame(); f.frame();
    expect(f.geometry.top).toBe(1800);
  });
  it("a layout shrink and its clamped top retain the pin", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame();
    f.geometry.height = 1000; f.geometry.top = 500;
    expect(f.end.scrolled()).toBe(true); f.end.changed(); f.frame();
    expect(f.end.active()).toBe(true); expect(f.geometry.top).toBe(500);
  });
  it("an explicit wheel/key/touch scroll-away cancels even a pending initial frame", () => {
    const f = fixture(); f.end.pin(); f.end.release(); f.geometry.top = 120;
    f.end.changed(); f.frame();
    expect(f.geometry.top).toBe(120); expect(f.writes()).toBe(0);
  });
  it("a scrollbar move away releases the pin and later growth preserves position", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame(); f.end.release(); f.geometry.top = 1100;
    expect(f.end.scrolled()).toBe(false);
    f.geometry.height += 800; f.end.changed(); f.frame();
    expect(f.geometry.top).toBe(1100);
  });
  it("browser scroll anchoring after render cannot be mistaken for user intent", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame();
    f.geometry.top = 300;
    expect(f.end.scrolled()).toBe(true); f.frame(); f.frame();
    expect(f.geometry.top).toBe(1500);
  });

  it("older-page anchoring remains in control after release", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame(); f.end.release();
    f.geometry.top = 20; f.geometry.height += 1000; f.geometry.top += 1000;
    f.end.changed(); f.frame();
    expect(f.geometry.top).toBe(1020);
  });
  it("a send after scrolling up re-pins later reply and layout growth", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame(); f.end.release(); f.geometry.top = 200;
    f.end.pin(); f.frame(); f.frame();
    f.geometry.height += 700; f.end.changed(); f.frame(); f.frame();
    expect(f.geometry.top).toBe(2200); expect(f.end.active()).toBe(true);
  });
  it("a transcript selection pauses follow writes and clearing it resumes", () => {
    let selected = true;
    const f = fixture(() => !selected);
    f.end.pin(); f.frame();
    expect(f.geometry.top).toBe(0);
    selected = false; f.end.changed(); f.frame(); f.frame();
    expect(f.geometry.top).toBe(1500);
    selected = true; f.geometry.height += 700; f.end.changed(); f.frame();
    expect(f.geometry.top).toBe(1500);
    selected = false; f.end.changed(); f.frame(); f.frame();
    expect(f.geometry.top).toBe(2200);
  });
  it("coalesces a resize burst into two frames and stops after disposal", () => {
    const f = fixture(); f.end.pin();
    for (let i = 0; i < 100; i++) f.end.changed();
    expect(f.queued.size).toBe(1); f.frame(); f.frame(); expect(f.writes()).toBe(1);
    f.end.changed(); f.end.dispose(); f.frame(); f.end.pin(); f.end.changed();
    expect(f.queued.size).toBe(0); expect(f.writes()).toBe(1);
  });
  it("reveals cached and delayed first pages only after their content is placed at the end", () => {
    let loaded=false, visible=false;
    const f=fixture(undefined,()=>{if(loaded)visible=true;});
    f.geometry.height=100;f.geometry.top=0;f.end.pin();f.frame();f.frame();
    expect(visible).toBe(false);
    loaded=true;f.geometry.height=9000;f.end.changed();
    expect(visible).toBe(false);f.frame();expect(visible).toBe(true);expect(f.geometry.top).toBe(8500);
    visible=false;f.geometry.height=4000;f.geometry.top=0;f.end.pin();
    expect(visible).toBe(false);f.frame();expect(visible).toBe(true);expect(f.geometry.top).toBe(3500);
  });
  it("streams while pinned without repeating writes for an unchanged layout", () => {
    const f = fixture(); f.end.pin(); f.frame(); f.frame();
    for (let n = 0; n < 60; n++) {
      const before = f.writes();
      f.geometry.height += 12 + n % 3;
      f.end.changed(); f.frame(); f.frame();
      expect(f.writes()).toBe(before + 1);
      expect(f.geometry.top + f.geometry.viewport).toBe(f.geometry.height);
      f.end.changed(); f.end.scrolled(); f.frame(); f.frame();
      expect(f.writes()).toBe(before + 1);
    }
  });
});

describe("local send detection", () => {
  it("does not jump for the initial load or a status update, only for a new send", () => {
    const initial = newLocalSends(undefined, ["local-1", "local-2"]);
    expect(initial.added).toEqual([]);
    expect(initial.baseline).toEqual(new Set(["local-1", "local-2"]));
    expect(newLocalSends(initial.baseline, ["local-1", "local-2"]).added).toEqual([]);
    expect(newLocalSends(initial.baseline, ["local-1", "local-2", "local-3"]).added).toEqual(["local-3"]);
  });
});
