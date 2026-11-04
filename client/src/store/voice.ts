import { createSignal } from "solid-js";
import * as bridge from "../bridge";
import { setSetting } from "./settings";
import { toast } from "./notices";
import { pauseMessagePlayback } from "./mediaPlayback";
import { tr } from "../i18n";

export type VoiceState = "idle" | "starting" | "recording" | "saving" | "preview" | "submitting" | "uncertain";
export interface VoiceShown {
  key: string; id: string; state: VoiceState; recording: boolean; hint: string | null;
  video_preview: boolean; voice_enabled: boolean; voice_icon: string; voice_label: string; voice_detail: string;
  mode_visible: boolean; mode_enabled: boolean; mode_icon: string; mode_label: string;
  cancel_visible: boolean; preview: string | null; play_label: string; play_enabled: boolean;
  discard_enabled: boolean; discard_label: string; providers: { label: string; profile: unknown }[];
  provider: number | null; provider_enabled: boolean; provider_placeholder: string; provider_tooltip: string;
  refresh_enabled: boolean; refresh_label: string; send_label: string; send_enabled: boolean;
  notice_visible: boolean; notice_label: string; notice_detail: string;
}
export type VoiceAsk = { kind: "cold_cache"; warning: string; ticket: number } | { kind: "discard"; session: string; ticket: number };
export type VoiceInput = { kind: "update"; input_visible: boolean } | { kind: "press" | "send" | "discard" | "refresh" }
  | { kind: "stop"; forget_send: boolean } | { kind: "choose_provider"; index: number }
  | { kind: "confirm"; ticket: number; confirmed: boolean } | { kind: "playing"; playing: boolean }
  | { kind: "inserted"; token: number; inserted: "refused" };
export type VoiceOut = { kind: "shown"; shown: VoiceShown } | { kind: "level"; rms: number; peak: number }
  | { kind: "preview"; mirrored: boolean } | { kind: "ask"; ask: VoiceAsk }
  | { kind: "message"; key: string; message: unknown } | { kind: "insert"; token: number; text: string; id: string }
  | { kind: "permission_recovery" | "pause_messages" | "stop_playback" }
  | { kind: "remember_profile"; profile: { machine_id?: string; profile_id?: string; profile_revision?: number } };
export interface VoiceContext { handle: number; key: string; visible: boolean; revision?: string }
export interface VoicePort {
  open(key: string, out: (event: VoiceOut) => void, frame: (bytes: Uint8Array) => void): Promise<void>;
  input(key: string, handle: number, input: VoiceInput): Promise<void>;
  close(key: string): Promise<void>;
  remember(profile: Extract<VoiceOut, { kind: "remember_profile" }>["profile"]): void;
  notice(text: string): void;
}

/** One mounted main composer, one driver, latest context only, and no retry of an action. */
export function createVoice(port: VoicePort, owner: string = crypto.randomUUID()) {
  const [shown, setShown] = createSignal<VoiceShown>();
  const [ask, setAsk] = createSignal<VoiceAsk>();
  const [permission, setPermission] = createSignal(false);
  const [error, setError] = createSignal("");
  const [frame, setFrame] = createSignal<Uint8Array>();
  const [mirrored, setMirrored] = createSignal(false);
  const [stopPlayback, setStopPlayback] = createSignal(0);
  let context: VoiceContext | undefined, scope = "", closed = false;
  let opened: Promise<void> | undefined, changing = false, pending = false;
  const input = async (command: VoiceInput) => {
    if (closed || !context || !opened) return;
    const target = context;
    try {
      await opened;
      if (!closed && context === target) await port.input(owner, target.handle, command.kind === "update" ? { ...command, input_visible: target.visible } : command);
    } catch (failure) { port.notice(String(failure)); }
  };
  const receive = (event: VoiceOut) => {
    if (closed) return;
    switch (event.kind) {
      case "shown": if (event.shown.key === context?.key) setShown(event.shown); break;
      case "ask":
        if (shown()?.key === context?.key && context) setAsk(event.ask);
        else void input({ kind: "confirm", ticket: event.ask.ticket, confirmed: false });
        break;
      case "permission_recovery": if (shown()?.key === context?.key && context) setPermission(true); break;
      case "preview": setMirrored(event.mirrored); break;
      case "stop_playback": setStopPlayback((n) => n + 1); break;
      case "pause_messages":
        pauseMessagePlayback();
        break;
      case "remember_profile": port.remember(event.profile); break;
      // Session delivery never inserts dictation into a text draft.
      case "insert": void input({ kind: "inserted", token: event.token, inserted: "refused" }); break;
    }
  };
  const update = async () => {
    if (changing || closed || !opened) return;
    changing = true;
    try {
      await opened;
      while (pending && !closed && context) {
        pending = false;
        const next = context;
        await port.input(owner, next.handle, { kind: "update", input_visible: next.visible });
      }
    } catch (failure) { setError(String(failure)); pending = false; }
    finally { changing = false; }
  };
  return {
    shown, ask, permission, frame, mirrored, error, stopPlayback, owner,
    follow(next: VoiceContext | undefined) {
      const identity = JSON.stringify(next);
      if (identity === scope || closed) return;
      const moved = next?.key !== context?.key;
      const previous = context;
      scope = identity; context = next;
      if (!next && previous && opened) {
        void opened.then(() => { if (!closed && !context) return port.input(owner, previous.handle, { kind: "stop", forget_send: true }); }).catch(() => {});
      }
      if (moved) { setShown(undefined); setFrame(undefined); setAsk(undefined); setPermission(false); setStopPlayback((n) => n + 1); }
      if (!next) return;
      if (!opened) {
        opened = port.open(owner, receive, (bytes) => { if (!closed && shown()?.key === context?.key && shown()?.video_preview) setFrame(bytes); });
        void opened.catch((failure) => setError(String(failure)));
      }
      pending = true; void update();
    },
    input,
    confirm(confirmed: boolean) { const current = ask(); setAsk(undefined); if (current) void input({ kind: "confirm", ticket: current.ticket, confirmed }); },
    closePermission() { setPermission(false); },
    async close() {
      closed = true; context = undefined; setShown(undefined); setFrame(undefined); setAsk(undefined);
      try { await opened; if (opened) await port.close(owner); } catch { /* Opening failed; no action to retry. */ }
    },
  };
}

export const voicePort: VoicePort = {
  open: (...args) => bridge.voiceOpen(...args), input: (...args) => bridge.voiceInput(...args), close: (...args) => bridge.voiceClose(...args),
  notice: (text) => { toast(tr("Voice notice"), text, "error", true); },
  remember: (profile) => {
    if (profile.machine_id !== undefined) void setSetting("speech/machine_id", profile.machine_id);
    if (profile.profile_id !== undefined) void setSetting("speech/profile", profile.profile_id);
    if (profile.profile_revision !== undefined) void setSetting("speech/profile_revision", profile.profile_revision);
  },
};

export type SessionVoice = ReturnType<typeof createVoice>;
