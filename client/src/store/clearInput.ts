import { createRoot, createSignal } from "solid-js";
import * as bridge from "../bridge";
import type { ClearOutcome, ComposerState } from "../bridge";
import type { ComposerView, Target } from "../types";
import type { Session } from "../ui/sessions/types";
import { draftKey } from "./draftKeys";

/**
 * Terminal's Clear input (zerus-wkj9l): the native composer of the session the
 * Terminal tab shows, read while it is shown, and the person's explicit clear
 * of exactly the text they confirmed. Nothing here clears on its own and no
 * delivery retry uses it.
 */
export interface ClearInputPort {
  read(target: Target): Promise<ComposerState | null>;
  clear(target: Target, binding: string, text: string): Promise<ClearOutcome>;
}

type Shown = Pick<Session, "key" | "catalogKey" | "computer" | "name" | "sessionId" | "status">;

/**
 * Whether the composer is delivering to session `s`: sending, waiting, a
 * desktop-service delivery pending or a local row still sending. The composer
 * names its session by draft key, not by the list row's key (G1 of #2094).
 */
export function delivering(view: ComposerView | undefined, s: Shown): boolean {
  if (!view) return false;
  const key = draftKey({ computer: s.computer, key: s.catalogKey, name: s.name, archive_id: null, session_id: s.sessionId ?? null });
  return view.key === key && (view.sending || !!view.delivery_pending || !!view.waiting || view.local.some((m) => m.status === "sending"));
}

/** The text Clear input may offer for `s` now: read as clearable for it, with its agent not working and nothing being sent. */
export function clearableText(read: { key: string; composer: ComposerState } | undefined, s: Shown | undefined,
  view: ComposerView | undefined, clearing: boolean): string | undefined {
  if (!read || !s || clearing || read.key !== s.key || !read.composer.clearable || s.status === "working" || delivering(view, s)) return undefined;
  return read.composer.text;
}

const same = (a: Target, b: Target) => a.computer === b.computer && a.name === b.name && a.run_id === b.run_id && a.conversation_id === b.conversation_id;

export function createClearInput(port: ClearInputPort) {
  const [state, setState] = createSignal<{ key: string; composer: ComposerState }>();
  const [clearing, setClearing] = createSignal(false);
  let current: { key: string; target: Target } | undefined;
  let serial = 0;

  async function refresh() {
    const at = current;
    const id = ++serial;
    if (!at) { setState(undefined); return; }
    const read = await port.read(at.target).catch(() => null);
    // A newer read or another session replaced this one meanwhile.
    if (id !== serial || current !== at) return;
    setState(read ? { key: at.key, composer: read } : undefined);
  }

  return {
    state, clearing, refresh,
    /** The Terminal tab shows `key`'s live session; undefined stops reading. */
    follow(key?: string, target?: Target) {
      if (key && target && current && current.key === key && same(current.target, target)) return;
      current = key && target ? { key, target } : undefined;
      setState(undefined);
      void refresh();
    },
    /**
     * Clears exactly `text`, read for `key` (the confirmation shows it), only
     * while `allowed` still offers that text: it is checked again as the
     * person confirms, before the request leaves.
     */
    async clear(key: string, text: string, allowed: () => string | undefined): Promise<ClearOutcome | undefined> {
      const at = current, read = state();
      if (!at || at.key !== key || !read || read.key !== key || !read.composer.clearable || clearing() || allowed() !== text) return undefined;
      setClearing(true);
      try {
        return await port.clear(at.target, read.composer.binding, text);
      } catch (error) {
        return { status: "uncertain", error: String(error), composer: null };
      } finally {
        setClearing(false);
        void refresh();
      }
    },
  };
}

export const clearInput = createRoot(() => createClearInput({ read: bridge.composerState, clear: bridge.clearInput }));
