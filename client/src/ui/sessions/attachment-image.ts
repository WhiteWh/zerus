import { createSignal, onCleanup } from "solid-js";
import type { ImageSize } from "../../types";

export function imageSize(value: unknown): ImageSize | undefined {
  if (!value || typeof value !== "object") return;
  const { width, height } = value as ImageSize;
  if (Number.isSafeInteger(width) && Number.isSafeInteger(height) && width > 0 && height > 0 && width * height <= 64_000_000) return { width, height };
}

/** Keep Qt's 96×64 cell; the image's final fitted box is known before decode. */
export function fittedImage(size: ImageSize | undefined) {
  if (!size) return { width: "96px", height: "64px" };
  const scale = Math.min(1, 96 / size.width, 64 / size.height);
  return { width: `${size.width * scale}px`, height: `${size.height * scale}px` };
}

/** A source handoff keeps the visible copy until its replacement is decoded.
 * A stale or failed read never blanks a working local preview. */
export function createImageHandoff(decode: (url: string) => Promise<void>) {
  const [url, setUrl] = createSignal<string>();
  let requested: string | undefined, revision = 0;
  onCleanup(() => { revision++; });
  return {
    url,
    local(value: string | undefined) { if (value && !url()) setUrl(value); },
    async remote(value: string | undefined) {
      if (!value || value === requested || value === url()) return;
      requested = value; const ticket = ++revision;
      try { await decode(value); if (ticket === revision) setUrl(value); }
      catch { if (ticket === revision) requested = undefined; }
    },
  };
}
