import { createSignal } from "solid-js";

/** One owner for a playing or pending recording, including composer previews.
 * Taking ownership cancels the previous read before it can begin playback. */
export function createPlaybackOwner() {
  let owner: { stop(): void; pause?: () => void } | undefined;
  return {
    take(stop: () => void, pause?: () => void) {
      const previous = owner;
      const mine = { stop, pause };
      owner = mine;
      previous?.stop();
      return {
        current: () => owner === mine,
        release() { if (owner === mine) owner = undefined; },
      };
    },
    pause() {
      if (owner?.pause) owner.pause();
      else { const previous = owner; owner = undefined; previous?.stop(); }
    },
    stop() { const previous = owner; owner = undefined; previous?.stop(); },
  };
}

export const mediaPlayback = createPlaybackOwner();
// Retain one loaded recording through pauses so scrolling preserves its seek position.
export const [retainedMediaId, setRetainedMediaId] = createSignal<string>();

/** Qt's registered message players pause together, also before capture. */
export function pauseMessagePlayback() {
  mediaPlayback.pause();
  if (typeof document !== "undefined") document.querySelectorAll<HTMLMediaElement>("audio,video").forEach((media) => media.pause());
}
