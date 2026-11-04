import { describe, expect, it } from "vitest";
import { createActivityAnchor } from "./activity-anchor";

function fixture() {
  const rows = Array.from({ length: 100 }, (_, i) => ({ key: `item-${i}`, height: 60 + i % 3 * 20 }));
  let scrollTop = 3257, released = true, serial = 0, writes = 0;
  const frames = new Map<number, () => void>();
  const all = () => { let top = -scrollTop; return rows.map((r) => { const row = { ...r, offset: top }; top += r.height; return row; }); };
  const visible = () => all().filter((r) => r.offset + r.height > 0 && r.offset < 500);
  const anchor = createActivityAnchor({ visible, offset: (key) => all().find((r) => r.key === key)?.offset, adjust: (delta) => { writes++; scrollTop += delta; }, scrollTop: () => scrollTop }, () => released, {
    request: (callback) => { const id = ++serial; frames.set(id, callback); return id; }, cancel: (id) => { frames.delete(id); },
  });
  const frame = () => { const current = [...frames.values()]; frames.clear(); current.forEach((run) => run()); };
  anchor.remember();
  return { anchor, rows, frame, visible, writes: () => writes, frames, top: () => scrollTop, scroll: (top: number) => { scrollTop = top; anchor.remember(); }, pin: () => { released = false; anchor.reset(); } };
}
const point = (f: ReturnType<typeof fixture>) => { const row = f.visible()[0]; return { key: row.key, offset: row.offset }; };

describe("Activity's released reading anchor", () => {
  it("keeps the exact mid-history row and pixel offset through appends and last-item growth", () => {
    const f = fixture(), before = point(f), top = f.top();
    for (let i = 0; i < 30; i++) {
      f.anchor.beforeChange(); f.rows.push({ key: `appended-${i}`, height: 120 });
      f.rows.at(-1)!.height += 500; f.anchor.changed(); f.frame();
      expect(point(f)).toEqual(before); expect(f.top()).toBe(top);
    }
    expect(f.writes()).toBe(0);
  });
  it("corrects growth above the viewport, including layout after the data update", () => {
    const f = fixture(), before = point(f);
    for (const growth of [125, 380, -100, .25]) {
      f.rows[3].height += growth; f.anchor.changed(); f.frame();
      expect(point(f)).toEqual(before);
    }
    expect(f.writes()).toBe(4);
  });
  it("anchors older-page inserts without counting simultaneous appends below", () => {
    const f = fixture(), before = point(f), top = f.top();
    f.rows.unshift({ key: "older-page", height: 740 });
    f.rows.push({ key: "new-message", height: 600 }); f.rows.at(-2)!.height += 330;
    f.anchor.changed(); f.frame();
    expect(point(f)).toEqual(before); expect(f.top()).toBe(top + 740);
  });
  it("retains its baseline across collapse changes and virtual measurement corrections", () => {
    const f = fixture(), before = point(f);
    f.rows[2].height = 20; f.anchor.changed();
    f.anchor.beforeChange(); // A second rebuild lands before the queued frame.
    f.rows[1].height = 350; f.anchor.changed(); f.frame();
    expect(point(f)).toEqual(before);
    f.rows[0].height += 160; f.anchor.changed(); f.frame();
    expect(point(f)).toEqual(before);
  });
  it("takes a fresh anchor only on user scroll and does not fight the end pin", () => {
    const f = fixture(); f.scroll(1681); const before = point(f);
    f.rows[1].height += 100; f.anchor.changed(); f.frame(); expect(point(f)).toEqual(before);
    f.anchor.changed(); f.pin(); f.rows[1].height += 100; f.anchor.changed(); f.frame();
    expect(f.writes()).toBe(1); expect(f.frames.size).toBe(0);
  });
  it("does not repeat an unrepresentable subpixel correction on each streamed event", () => {
    let offset = -10.25, writes = 0;
    const frames = new Map<number, () => void>(); let id = 0;
    const anchor = createActivityAnchor({ visible: () => [{ key: "row", offset }], offset: () => offset, adjust: (delta) => { writes++; offset -= Math.round(delta); } }, () => true, { request: (run) => { frames.set(++id, run); return id; }, cancel: (at) => { frames.delete(at); } });
    anchor.remember(); offset += 1.4;
    for (let i = 0; i < 20; i++) { anchor.changed(); const callbacks = [...frames.values()]; frames.clear(); callbacks.forEach((run) => run()); }
    expect(writes).toBe(1); expect(Math.abs(offset + 10.25)).toBeLessThan(.5);
  });
  it("uses a surviving visible row if a group disappears, and cancels on dispose", () => {
    const f = fixture(), second = f.visible()[1];
    const before = { key: second.key, offset: second.offset };
    f.rows.splice(f.rows.findIndex((r) => r.key === f.visible()[0].key), 1);
    f.anchor.changed(); f.frame();
    expect(f.visible().find((r) => r.key === before.key)?.offset).toBe(before.offset);
    f.anchor.changed(); f.anchor.dispose(); f.rows[0].height += 100; f.anchor.changed(); f.frame();
    expect(f.writes()).toBe(1);
  });
});

it("never corrects a layout or queued verification frame while a scroll owns the viewport", () => {
  let offset=-40, writes=0, busy=false;const frames=new Map<number,()=>void>();let id=0;
  const anchor=createActivityAnchor({visible:()=>[{key:"reading",offset}],offset:()=>offset,adjust:(delta)=>{writes++;offset-=delta;}},()=>true,{request:run=>{frames.set(++id,run);return id;},cancel:key=>{frames.delete(key);}},()=>!busy);
  anchor.remember();anchor.changed();busy=true;offset+=100;
  anchor.beforeChange();anchor.changed();for(const run of frames.values())run();frames.clear();
  expect(writes).toBe(0);expect(offset).toBe(60);
  busy=false;anchor.changed();expect(writes).toBe(1);expect(offset).toBe(-40);
});

it("adopts a browser scroll accepted before its scroll event, then preserves later layout changes", () => {
  let top = 500, above = 100, writes = 0;
  const anchor = createActivityAnchor({
    visible: () => [{ key: "reading", offset: above - top }],
    offset: () => above - top,
    adjust: delta => { writes++; top += delta; },
    scrollTop: () => top,
  }, () => true);
  anchor.remember();
  // The compositor moved before the queued DOM scroll handler could remember
  // it. A streamed layout callback must not restore the prior viewport.
  top = 640;
  anchor.beforeChange();
  expect(top).toBe(640);
  expect(writes).toBe(0);
  above += 80;
  anchor.beforeChange();
  expect(top).toBe(720);
  expect(writes).toBe(1);
  anchor.dispose();
});

it("a queued verification frame also yields to a newly accepted browser position", () => {
  let top = 400, writes = 0, serial = 0;
  const frames = new Map<number, () => void>();
  const anchor = createActivityAnchor({ visible: () => [{ key: "reading", offset: 100 - top }], offset: () => 100 - top,
    scrollTop: () => top, adjust: delta => { writes++; top += delta; } }, () => true,
    { request: run => { frames.set(++serial, run); return serial; }, cancel: id => { frames.delete(id); } });
  anchor.remember(); anchor.changed();
  top = 540;
  for (const run of frames.values()) run();
  expect(top).toBe(540); expect(writes).toBe(0);
  anchor.dispose();
});
