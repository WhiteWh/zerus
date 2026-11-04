/** Layout settling at Activity's end, shared by initial placement and follow growth. */
export interface ScrollGeometry { top: number; height: number; viewport: number }
export interface AnimationFrames { request(callback: () => void): number; cancel(id: number): void }
export const animationFrames: AnimationFrames = { request: (callback) => requestAnimationFrame(callback), cancel: (id) => cancelAnimationFrame(id) };

/**
 * Pin after rendering, then verify once in the next frame. ResizeObserver's
 * later content/viewport changes coalesce into another pair of frames.
 * Layout and browser scroll anchoring never revoke the pin. The owner
 * releases it on explicit user scroll intent or before restoring an older page.
 */
export function createActivityEnd(read: () => ScrollGeometry, writeEnd: () => void, frames: AnimationFrames = animationFrames, canWrite: () => boolean = () => true, placed: () => void = () => {}) {
  let active = false, disposed = false, epoch = 0;
  let frame: number | undefined;
  let last: ScrollGeometry | undefined;
  const cancel = () => { if (frame !== undefined) frames.cancel(frame); frame = undefined; };
  const schedule = (verify = true) => {
    if (!active || disposed || frame !== undefined) return;
    const mine = epoch;
    frame = frames.request(() => {
      if (mine !== epoch || !active || disposed) return;
      frame = undefined;
      if (!canWrite()) return;
      const now = read();
      // A stable verification frame must not write the same position again:
      // WebKit sends overflow scrolling through its asynchronous compositor.
      if (Math.abs(now.top - Math.max(0, now.height - now.viewport)) > 1) writeEnd();
      last = read();
      if (Math.abs(last.top - Math.max(0, last.height - last.viewport)) <= 1) placed();
      if (verify) schedule(false);
    });
  };
  const release = () => { active = false; epoch++; cancel(); last = undefined; };
  return {
    active: () => active,
    pin() { if (disposed) return; release(); active = true; schedule(); },
    changed() { schedule(); },
    /**
     * A resize, written now: ResizeObserver delivers after layout and before
     * paint, so the frame paints the settled end instead of one shifted frame
     * that the next frame corrects. Never during a user scroll (canWrite).
     */
    settle() {
      if (!active || disposed) return;
      if (canWrite()) {
        const now = read();
        if (Math.abs(now.top - Math.max(0, now.height - now.viewport)) > 1) writeEnd();
        last = read();
      }
      schedule();
    },
    /** True while a scroll belongs to initial placement or a layout change. */
    scrolled() {
      if (!active || disposed) return false;
      const now = read();
      if (!last || now.height !== last.height || now.viewport !== last.viewport
        || now.height - now.top - now.viewport > 1) schedule();
      return true;
    },
    release,
    dispose() { release(); disposed = true; },
  };
}

export function newLocalSends(baseline: ReadonlySet<string> | undefined, current: readonly string[]): { added: string[]; baseline: Set<string> } {
  return { added: baseline ? current.filter((id) => !baseline.has(id)) : [], baseline: new Set(current) };
}
