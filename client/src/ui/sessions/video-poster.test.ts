import { describe, expect, it, vi } from "vitest";
import { posterUrl, visiblePoster, type PosterPort } from "./video-poster";
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string, scheme: string) => `${scheme}://localhost/${path}` }));

function fixture() {
  let visibility!: (shown: boolean) => void;
  let finish!: (blob: Blob) => void;
  const ready = vi.fn();
  const port: PosterPort = {
    observe: vi.fn((_, next) => { visibility = next; return vi.fn(); }),
    read: vi.fn(() => new Promise<Blob>((resolve) => { finish = resolve; })),
    objectUrl: vi.fn(() => "blob:private-poster"), revoke: vi.fn(), decode: vi.fn(async () => {}),
  };
  const gate = visiblePoster({} as Element, "zerus-file://localhost/poster", ready, port);
  gate.setEnabled(true);
  return { port, ready, close: gate.dispose, enabled: gate.setEnabled, visible: (v: boolean) => visibility(v), finish: () => finish(new Blob([new Uint8Array([255,216,255,217])], { type: "image/jpeg" })) };
}
describe("visible video posters", () => {
  it("keeps exact computer/key/message identities out of filesystem paths", () => {
    const url = new URL(posterUrl("example-peer\nname:sh/example/a b", "message-1")!);
    expect(url.pathname).toBe("/poster"); expect(url.searchParams.get("computer")).toBe("example-peer");
    expect(url.searchParams.get("key")).toBe("name:sh/example/a b"); expect(url.searchParams.get("message_id")).toBe("message-1");
    expect(new URL(posterUrl("\nrun:one", "message")!).searchParams.get("computer")).toBe("");
    expect(posterUrl("missing-key", "message")).toBeUndefined();
  });
  it("does no read offscreen, attempts once on visibility and owns only one object URL", async () => {
    const f = fixture(); f.visible(false); expect(f.port.read).not.toHaveBeenCalled();
    f.visible(true); f.visible(true); expect(f.port.read).toHaveBeenCalledTimes(1);
    f.finish(); await vi.waitFor(() => expect(f.ready).toHaveBeenCalledWith("blob:private-poster"));
    f.visible(false); f.visible(true); expect(f.port.read).toHaveBeenCalledTimes(1);
    f.close(); expect(f.port.revoke).toHaveBeenCalledExactlyOnceWith("blob:private-poster");
  });
  it("waits for Activity placement and current geometry, not a stale observer callback", () => {
    const f = fixture(); f.enabled(false); f.visible(true); expect(f.port.read).not.toHaveBeenCalled();
    f.port.inView = () => false; f.enabled(true); expect(f.port.read).not.toHaveBeenCalled();
    f.port.inView = () => true; f.visible(true); expect(f.port.read).toHaveBeenCalledTimes(1); f.close();
  });
  it("cancels unfinished work on leaving, ignores a late reply and never retries it", async () => {
    const f = fixture(); f.visible(true); f.visible(false);
    const signal = vi.mocked(f.port.read).mock.calls[0][1]; expect(signal.aborted).toBe(true);
    f.finish(); await Promise.resolve(); f.visible(true); expect(f.port.read).toHaveBeenCalledTimes(1);
    expect(f.port.objectUrl).not.toHaveBeenCalled(); expect(f.ready).not.toHaveBeenCalled(); f.close();
  });
  it("unmount during decode releases once and never publishes a late poster", async () => {
    const f = fixture(); let decoded!: () => void;
    f.port.decode = vi.fn(() => new Promise<void>((resolve) => { decoded = resolve; }));
    f.visible(true); f.finish(); await vi.waitFor(() => expect(f.port.decode).toHaveBeenCalled());
    f.close(); decoded(); await Promise.resolve();
    expect(f.ready).not.toHaveBeenCalled(); expect(f.port.revoke).toHaveBeenCalledExactlyOnceWith("blob:private-poster");
  });
  it("refusal keeps the existing placeholder without an error or another request", async () => {
    const f = fixture(); f.port.read = vi.fn(async () => { throw Error("missing"); });
    f.visible(true); await Promise.resolve(); await Promise.resolve(); f.visible(false); f.visible(true);
    expect(f.ready).not.toHaveBeenCalled(); expect(f.port.read).toHaveBeenCalledTimes(1); f.close();
  });
});
