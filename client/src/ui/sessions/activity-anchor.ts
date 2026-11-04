import { animationFrames, type AnimationFrames } from "./activity-end";

/** A stable rendered row and its top relative to the viewport's top. */
export interface ReadingAnchor { key: string; offset: number }
export interface ReadingRows {
  visible(): readonly ReadingAnchor[];
  offset(key: string): number | undefined;
  adjust(delta: number): void;
  /** Accepted browser position, which may change before its DOM scroll event. */
  scrollTop?(): number;
}

/**
 * The released end pin has one scroll owner: this row anchor. Capture on
 * user scroll, retain it across updates, then correct only its pixel drift.
 * A virtualizer can supply measured offsets for a key before it remounts.
 */
export function createActivityAnchor(rows: ReadingRows, released: () => boolean, frames: AnimationFrames = animationFrames, canWrite: () => boolean = () => true) {
  let corrected: ReadingAnchor | undefined;
  let anchors: readonly ReadingAnchor[] = [], frame: number | undefined, disposed = false;
  let observedTop: number | undefined;
  const cancel = () => { if (frame !== undefined) frames.cancel(frame); frame = undefined; };
  const remember = () => {
    if (disposed || !released()) return;
    anchors = rows.visible().slice(0, 8);
    corrected = undefined;
    observedTop = rows.scrollTop?.();
  };
  const correct = () => {
    if (disposed || !released() || !canWrite()) return;
    const top = rows.scrollTop?.();
    if (top !== undefined && observedTop !== undefined && Math.abs(top - observedTop) > .01) {
      // A source/measurement callback can beat the browser's queued scroll
      // event. Adopt its accepted position before restoring any named row.
      // This uses neither total content height nor an estimated scroll delta.
      remember();
      return;
    }
    for (const anchor of anchors) {
      const offset = rows.offset(anchor.key);
      if (offset === undefined) continue;
      const delta = offset - anchor.offset;
      // Some webviews quantize scrollTop. Do not keep writing the same
      // unrepresentable fraction on every streamed event or verify frame.
      if (Math.abs(delta) > .01 && (corrected?.key !== anchor.key || Math.abs(corrected.offset - offset) > .01)) {
        rows.adjust(delta);
        observedTop = rows.scrollTop?.();
        corrected = { key: anchor.key, offset: rows.offset(anchor.key) ?? offset };
      }
      return;
    }
  };
  const changed = () => {
    if (disposed || !released() || !canWrite() || !anchors.length) return;
    correct(); // ResizeObserver and mutation delivery run before paint.
    if (frame !== undefined) return;
    frame = frames.request(() => { frame = undefined; correct(); });
  };
  return {
    /** Keep a few visible successors only as fallbacks if a row is removed. */
    remember,
    /** Run before reconciliation, so back-to-back changes cannot adopt drift. */
    beforeChange: correct,
    changed,
    reset() { cancel(); anchors = []; corrected = undefined; observedTop = undefined; },
    dispose() { cancel(); anchors = []; disposed = true; },
  };
}

/** Direct transcript rows today; the virtualizer uses the same key contract. */
export function activityReadingRows(scroller: HTMLElement): ReadingRows {
  const top = () => scroller.getBoundingClientRect().top + scroller.clientTop;
  return {
    visible() {
      const column = scroller.querySelector(".act-col");
      const rows = column?.querySelectorAll<HTMLElement>("[id^='item-']");
      if (!rows?.length) return [];
      const edge = top(), bottom = edge + scroller.clientHeight;
      let lo = 0, hi = rows.length;
      while (lo < hi) { const mid = (lo + hi) >>> 1; if (rows[mid].getBoundingClientRect().bottom <= edge) lo = mid + 1; else hi = mid; }
      const visible: ReadingAnchor[] = [];
      for (let i = lo; i < rows.length && visible.length < 8; i++) {
        const rect = rows[i].getBoundingClientRect(); if (rect.top >= bottom) break;
        visible.push({ key: rows[i].id, offset: rect.top - edge });
      }
      return visible;
    },
    offset(key) { const row = scroller.ownerDocument.getElementById(key); return row && scroller.contains(row) ? row.getBoundingClientRect().top - top() : undefined; },
    adjust(delta) { scroller.scrollTo({ top: scroller.scrollTop + delta, behavior: "instant" }); },
    scrollTop: () => scroller.scrollTop,
  };
}
