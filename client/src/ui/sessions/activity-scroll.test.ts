import { expect, it } from "vitest";
import { createActivityScroll, type ScrollIdleHost } from "./activity-scroll";
import { createActivityAnchor } from "./activity-anchor";
import { createActivityEnd } from "./activity-end";

function clock() {
  let time = 0, id = 0;
  const frames = new Map<number, () => void>(), timers = new Map<number, { at: number; run: () => void }>();
  const host: ScrollIdleHost = {
    request: (run) => { frames.set(++id, run); return id; }, cancel: (key) => { frames.delete(key); },
    later: (run, ms) => { const key=++id; timers.set(key,{ at: time+ms, run }); return () => { timers.delete(key); }; },
  };
  return { host, advance(ms: number) { time+=ms; for (const [key,t] of [...timers]) if(t.at<=time) { timers.delete(key); t.run(); } }, frame() { const now=[...frames.values()]; frames.clear(); now.forEach(run=>run()); } };
}

it("makes zero anchor or end writes during scrolling and commits only the latest layout at idle", () => {
  const c=clock(); let top=420, above=100, last=100, writes=0, commits=0;
  const anchor=createActivityAnchor({ visible:()=>[{key:"reading",offset:above-top}],offset:()=>above-top,adjust:(delta)=>{writes++;top+=delta;} },()=>!end.active(),c.host,()=>!motion.active());
  const end=createActivityEnd(()=>({top,height:10000+above+last,viewport:500}),()=>{writes++;},c.host,()=>!motion.active());
  const motion=createActivityScroll(()=>top,(flush)=>{anchor.remember();flush();anchor.changed();},c.host);
  anchor.remember(); motion.intent(); end.release();
  for(let n=1;n<=60;n++) {
    top+=n<30?-3:2; motion.moved(); anchor.remember();
    // Each streamed projection includes the full latest page and growing item.
    motion.update(()=>{commits++;above=100+n*4;last=100+n*20;anchor.changed();end.changed();});
    anchor.beforeChange();anchor.changed();end.changed();c.advance(40);c.frame();
    expect(writes).toBe(0);expect(commits).toBe(0);
  }
  const finalTop=top;c.advance(250);c.frame();expect(commits).toBe(0);c.frame();c.frame();
  expect(commits).toBe(1);expect(top).toBe(finalTop+240);expect(last).toBe(1300);expect(writes).toBe(1);
});

it("re-arms for momentum scroll events and waits for two compositor-stable frames",()=>{
  const c=clock();let top=10, settled=0;
  const motion=createActivityScroll(()=>top,flush=>{settled++;flush();},c.host);
  motion.intent();
  for(let n=0;n<20;n++){c.advance(200);top+=2;motion.moved();c.frame();expect(settled).toBe(0);}
  c.advance(250);top++;c.frame();c.frame();expect(settled).toBe(0);
  c.advance(250);c.frame();top++;c.frame();expect(settled).toBe(0);
  c.advance(250);c.frame();c.frame();expect(settled).toBe(1);expect(motion.active()).toBe(false);
});

it("holds scrollbar and selection drags until release, and drops obsolete layouts on reset",()=>{
  const c=clock();let applied=0;
  const motion=createActivityScroll(()=>10,flush=>flush(),c.host);
  motion.hold();motion.update(()=>{applied++;});
  c.advance(10000);c.frame();c.frame();expect(motion.active()).toBe(true);expect(applied).toBe(0);
  motion.release();c.advance(250);c.frame();c.frame();expect(applied).toBe(1);
  motion.intent();motion.update(()=>{applied++;});motion.reset();c.advance(1000);c.frame();c.frame();expect(applied).toBe(1);
  motion.intent();motion.update(()=>{applied++;});motion.dispose();c.advance(1000);c.frame();c.frame();expect(applied).toBe(1);
});

it("holds a persistent selection after pointerup, applies only the latest layout, then resumes the pin", () => {
  const c = clock();
  let top = 1500, height = 2000, commits = 0, writes = 0;
  const end = createActivityEnd(() => ({ top, height, viewport: 500 }), () => { writes++; top = height - 500; }, c.host, () => !motion.active());
  const motion = createActivityScroll(() => top, (flush) => { flush(); end.changed(); }, c.host);
  end.pin(); c.frame(); c.frame();
  motion.hold(); motion.selection(true); motion.release();
  for (let n = 1; n <= 20; n++) {
    motion.update(() => { commits++; height = 2000 + n * 100; });
    end.changed(); c.advance(500); c.frame(); c.frame();
    expect(top).toBe(1500); expect(height).toBe(2000);
    expect(writes).toBe(0); expect(commits).toBe(0);
  }
  motion.selection(false); c.advance(249); c.frame(); c.frame(); expect(commits).toBe(0);
  c.advance(1); c.frame(); c.frame(); c.frame(); c.frame();
  expect(commits).toBe(1); expect(height).toBe(4000); expect(top).toBe(3500); expect(writes).toBe(1);
});

it("a programmatic selection also holds a released reading anchor until the range clears", () => {
  const c = clock(); let top = 400, above = 100, writes = 0;
  const anchor = createActivityAnchor({ visible: () => [{ key: "reading", offset: above - top }], offset: () => above - top, adjust: (delta) => { writes++; top += delta; } }, () => true, c.host, () => !motion.active());
  const motion = createActivityScroll(() => top, (flush) => { anchor.remember(); flush(); anchor.changed(); }, c.host);
  anchor.remember(); motion.selection(true);
  motion.update(() => { above = 350; anchor.changed(); });
  c.advance(10000); c.frame(); c.frame(); anchor.changed();
  expect(motion.active()).toBe(true); expect(writes).toBe(0); expect(top).toBe(400);
  motion.selection(false); c.advance(250); c.frame(); c.frame(); c.frame();
  expect(top).toBe(650); expect(writes).toBe(1);
  motion.selection(true); motion.update(() => { above = 900; }); motion.reset();
  motion.selection(false); c.advance(1000); c.frame(); c.frame(); expect(above).toBe(350);
});

it("opens a measured long history and scrolls slowly without new events or corrective writes",()=>{
  const c=clock(), rows=Array.from({length:200},(_,n)=>({key:`row-${n}`,height:20+n%7*127}));
  let top=rows.reduce((sum,row)=>sum+row.height,0)-600,writes=0;
  const absolute=new Map<string,number>();let y=0;for(const row of rows){absolute.set(row.key,y);y+=row.height;}
  const visible=()=>rows.map(row=>({key:row.key,offset:absolute.get(row.key)!-top,height:row.height})).filter(row=>row.offset+row.height>0&&row.offset<600);
  const motion=createActivityScroll(()=>top,flush=>{anchor.remember();flush();anchor.changed();},c.host);
  const anchor=createActivityAnchor({visible,offset:key=>absolute.get(key)!-top,adjust:delta=>{writes++;top+=delta;}},()=>true,c.host,()=>!motion.active());
  anchor.remember();
  for(let n=0;n<200;n++){motion.intent();top-=3;motion.moved();anchor.remember();anchor.beforeChange();anchor.changed();c.advance(100);c.frame();const row=visible()[0];expect(row.offset+top).toBe(absolute.get(row.key));expect(writes).toBe(0);}
  c.advance(250);c.frame();c.frame();c.frame();expect(writes).toBe(0);
});

it("reports a deferred layout until the quiet period applies it, and not for a gesture alone", () => {
  const c = clock();
  const motion = createActivityScroll(() => 10, (flush) => flush(), c.host);
  motion.update(() => {});
  expect(motion.deferred()).toBe(false);
  motion.intent();
  expect(motion.active()).toBe(true);
  expect(motion.deferred()).toBe(false);
  motion.update(() => {});
  expect(motion.deferred()).toBe(true);
  c.advance(250); c.frame(); c.frame();
  expect(motion.active()).toBe(false);
  expect(motion.deferred()).toBe(false);
  motion.selection(true); motion.update(() => {});
  expect(motion.deferred()).toBe(true);
  motion.reset();
  expect(motion.deferred()).toBe(false);
});
