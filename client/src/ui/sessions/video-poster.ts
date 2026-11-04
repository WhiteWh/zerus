import { convertFileSrc } from "@tauri-apps/api/core";

export function posterUrl(sourceId: string, message: string): string | undefined {
  const split = sourceId.indexOf("\n");
  if (split < 0 || !message) return;
  const query = new URLSearchParams({ computer: sourceId.slice(0, split), key: sourceId.slice(split + 1), message_id: message });
  return `${convertFileSrc("poster", "zerus-file")}?${query}`;
}

export interface PosterPort {
  observe(card: Element, visible: (shown: boolean) => void): () => void;
  inView?(card: Element): boolean;
  read(url: string, signal: AbortSignal): Promise<Blob>;
  objectUrl(blob: Blob): string;
  revoke(url: string): void;
  decode(url: string): Promise<void>;
}
const native: PosterPort = {
  inView(card) {
    const rect = card.getBoundingClientRect();
    const clip = card.closest(".act-scroll")?.getBoundingClientRect();
    return getComputedStyle(card).visibility === "visible" && rect.width > 0 && rect.height > 0
      && rect.bottom > Math.max(0, clip?.top ?? 0) && rect.top < Math.min(innerHeight, clip?.bottom ?? innerHeight)
      && rect.right > Math.max(0, clip?.left ?? 0) && rect.left < Math.min(innerWidth, clip?.right ?? innerWidth);
  },
  observe(card, visible) {
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) if (entry.target === card) visible(entry.isIntersecting);
    });
    observer.observe(card); return () => observer.disconnect();
  },
  async read(url, signal) {
    const response = await fetch(url, { signal, cache: "no-store", credentials: "omit" });
    if (!response.ok || response.headers.get("content-type") !== "image/jpeg") throw Error("poster unavailable");
    const blob = await response.blob();
    if (!blob.size || blob.size > 8192) throw Error("poster unavailable");
    return blob;
  },
  objectUrl: (blob) => URL.createObjectURL(blob),
  revoke: (url) => URL.revokeObjectURL(url),
  async decode(url) { const image = new Image(); image.src = url; await image.decode(); },
};

/** One request per mounted card. Bytes live only in its DOM/object URL, never a store. */
export function visiblePoster(card: Element, url: string, ready: (url: string) => void, port = native) {
  let attempted = false, visible = false, intersecting = false, enabled = false, finished = false, disposed = false, object = "";
  const controller = new AbortController();
  const update = () => {
    visible = enabled && intersecting && (port.inView?.(card) ?? true);
    if (!visible) { if (attempted && !finished) controller.abort(); return; }
    if (attempted || disposed) return;
    attempted = true;
    void port.read(url, controller.signal).then(async (blob) => {
      if (disposed || !visible || controller.signal.aborted) return;
      const made = port.objectUrl(blob); object = made;
      await port.decode(made);
      if (disposed || !visible || controller.signal.aborted) { if (object === made) { port.revoke(made); object = ""; } return; }
      finished = true; ready(made);
    }).catch(() => { if (object) { port.revoke(object); object = ""; } });
  };
  const disconnect = port.observe(card, (shown) => { intersecting = shown; update(); });
  return {
    setEnabled(shown: boolean) { enabled = shown; update(); },
    dispose() { disposed = true; disconnect(); controller.abort(); if (object) { port.revoke(object); object = ""; } },
  };
}
