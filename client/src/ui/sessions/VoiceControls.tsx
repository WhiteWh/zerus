import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import * as bridge from "../../bridge";
import { flag, setSetting } from "../../store/settings";
import { toast } from "../../store/notices";
import type { SessionVoice } from "../../store/voice";
import { recordingBlob } from "./voice-playback";
import { mediaPlayback } from "../../store/mediaPlayback";
import { tr, trc } from "../../i18n";

const icon = (name: string) => ({ microphone: "mic", stop: "square" }[name] ?? name);

/** Mockup Composer.tsx:90-98; Qt decides Send versus Stop from the recording preference. */
export function VoiceToolbar(props: { voice: SessionVoice; compact: boolean }) {
  const shown = () => props.voice.shown();
  const recording = () => shown()?.state === "recording";
  const video = () => flag("speech/video_mode");
  const [changingMode, setChangingMode] = createSignal(false);
  const [support, setSupport] = createSignal<{ available: boolean; reason: string | null }>();
  onMount(() => { void bridge.voiceVideo().then(setSupport).catch(() => {}); });
  const title = () => recording() ? shown()?.voice_detail
    : props.voice.error() || (video() && support()?.available === false ? support()?.reason : undefined)
    || (shown()?.voice_enabled === false ? shown()?.voice_detail : undefined)
    || (video() ? tr("Click to start a video recording.") : tr("Click to start a voice recording."));
  return <>
    <button type="button" class="tchip rec-slot" classList={{ rec: recording() }} title={title()}
      disabled={changingMode() || !shown()?.voice_enabled || (!recording() && video() && support()?.available === false)}
      onClick={() => { void props.voice.input({ kind: "press" }); }}>
      <Icon name={recording() ? icon(shown()?.voice_icon ?? "stop") : video() ? "video" : "mic"} size={13} />
      {recording() ? shown()?.voice_label : shown()?.state === "starting" || shown()?.state === "saving" ? shown()?.voice_label : video() ? tr("Video") : tr("Voice")}
    </button>
    <button type="button" class="tchip icon" disabled={changingMode() || !shown()?.mode_enabled || recording()}
      aria-label={video() ? tr("Switch to voice recording") : tr("Switch to video recording")} data-tip={video() ? tr("Switch to voice recording") : tr("Switch to video recording")}
      onClick={() => {
        setChangingMode(true);
        void Promise.resolve(setSetting("speech/video_mode", !video())).then(() => props.voice.input({ kind: "update", input_visible: true }))
          .finally(() => setChangingMode(false));
      }}><Icon name={video() ? "mic" : "video"} size={13} /></button>
    <Show when={shown()?.cancel_visible}>
      <button type="button" class="tchip" title={tr("Cancel recording")} onClick={() => { void props.voice.input({ kind: "discard" }); }}><Icon name="x" size={13} /><Show when={!props.compact}>{tr("Cancel recording")}</Show></button>
    </Show>
    <Show when={shown()?.notice_visible}>
      <span class="tchip warn" title={shown()?.notice_detail}><Icon name="mic" size={13} /><Show when={!props.compact}>{shown()?.notice_label}</Show></span>
    </Show>
  </>;
}

/** Qt's preview controls, in the placement approved by webui-ac for .9.38. */
export function VoicePreview(props: { voice: SessionVoice }) {
  const voice = props.voice, shown = voice.shown;
  let player: HTMLMediaElement | undefined;
  let objectUrl = "", generation = 0, busy = false;
  let claim: ReturnType<typeof mediaPlayback.take> | undefined;
  const [playing, setPlaying] = createSignal(false);
  const [videoFile, setVideoFile] = createSignal("");
  const [jpeg, setJpeg] = createSignal("");
  const clear = () => {
    claim?.release(); claim = undefined;
    generation++; player?.pause(); setPlaying(false);
    if (player) { player.removeAttribute("src"); player.load(); }
    if (objectUrl) URL.revokeObjectURL(objectUrl);
    objectUrl = "";
    setVideoFile("");
  };
  const recordingId = createMemo(() => shown()?.id ?? "");
  createEffect(on(recordingId, clear));
  createEffect(() => { void voice.stopPlayback(); generation++; player?.pause(); setPlaying(false); });
  createEffect(() => {
    const bytes = voice.frame();
    if (!bytes?.length || bytes.length > 1024 * 1024) { setJpeg(""); return; }
    // Existing img-src permits data: previews. One reader/frame at most;
    // superseded JPEGs are aborted, with no CSP or protocol change.
    const reader = new FileReader();
    reader.onload = () => setJpeg(typeof reader.result === "string" ? reader.result : "");
    reader.readAsDataURL(new Blob([new Uint8Array(bytes)], { type: "image/jpeg" }));
    onCleanup(() => { reader.onload = null; reader.abort(); });
  });
  onCleanup(clear);
  const play = async () => {
    if (busy) return;
    if (playing()) { player?.pause(); return; }
    const id = shown()?.id;
    if (!id) return;
    if (!claim?.current()) claim = mediaPlayback.take(clear);
    const mine = generation;
    busy = true;
    try {
      if (!objectUrl) {
        const bytes = await bridge.voicePreview(voice.owner, id);
        if (mine !== generation || shown()?.id !== id) return;
        objectUrl = URL.createObjectURL(recordingBlob(bytes, !!shown()?.video_preview));
      }
      if (shown()?.video_preview) setVideoFile(objectUrl);
      await Promise.resolve();
      if (mine === generation && claim?.current() && player) { if (player.src !== objectUrl) player.src = objectUrl; await player.play(); }
    } catch (failure) { if (mine === generation) { state(false); toast(tr("Voice notice"), tr("Could not play recording: %1", String(failure)), "error", true); } }
    finally { busy = false; }
  };
  const state = (on: boolean) => { setPlaying(on); void voice.input({ kind: "playing", playing: on }); };
  return <>
    <Show when={shown()?.video_preview}>
      <div class="cmp-voice-round">
        <Show when={videoFile()} fallback={<Show when={jpeg()}><img src={jpeg()} alt={tr("Live video preview")} style={{ transform: voice.mirrored() ? "scaleX(-1)" : undefined }} /></Show>}>
          <video ref={(el) => { player = el; }} playsinline preload="metadata" poster={jpeg()} src={videoFile()} onPlaying={() => state(true)} onPause={() => state(false)} onEnded={() => state(false)} />
        </Show>
      </div>
    </Show>
    <Show when={shown()?.hint}><p class="cmp-voice-hint muted">{shown()?.state === "recording" && shown()?.voice_icon === "send" ? tr("Recording. Send, or press Space / Esc to stop and preview.") : shown()?.hint}</p></Show>
    <Show when={shown()?.preview}>
      <div class="cmp-voice-preview">
        <span class="muted">{shown()?.preview}</span>
        <button type="button" class="btn small" disabled={!shown()?.play_enabled || (shown()?.state !== "preview" && shown()?.state !== "uncertain")} onClick={() => { void play(); }}>{playing() ? trc("media playback", "Pause") : trc("media playback", "Play")}</button>
        <select class="field" aria-label={tr("Voice provider")} title={shown()?.provider_tooltip} value={shown()?.provider ?? -1} disabled={!shown()?.provider_enabled}
          onChange={(event) => { void voice.input({ kind: "choose_provider", index: Number(event.currentTarget.value) }); }}>
          <Show when={shown()?.provider == null}><option value={-1}>{shown()?.provider_placeholder}</option></Show>
          <For each={shown()?.providers ?? []}>{(provider, i) => <option value={i()}>{provider.label}</option>}</For>
        </select>
        <button type="button" class="btn small" title={tr("Refresh available voice providers without recording again")} disabled={!shown()?.refresh_enabled} onClick={() => { void voice.input({ kind: "refresh" }); }}>{shown()?.refresh_label}</button>
        <button type="button" class="btn small" disabled={!shown()?.discard_enabled} onClick={() => { void voice.input({ kind: "discard" }); }}>{shown()?.discard_label}</button>
        <button type="button" class="btn small primary" disabled={!shown()?.send_enabled} onClick={() => { void voice.input({ kind: "send" }); }}>{shown()?.send_label}</button>
      </div>
    </Show>
    <Show when={!shown()?.video_preview}><audio ref={(el) => { player = el; }} onPlaying={() => state(true)} onPause={() => state(false)} onEnded={() => state(false)} /></Show>
    <VoiceDialogs voice={voice} />
  </>;
}

function VoiceDialogs(props: { voice: SessionVoice }) {
  let dialog!: HTMLDialogElement;
  const voice = props.voice;
  const [permission, setPermission] = createSignal<{ text: string; can_open: boolean }>();
  let previousFocus: HTMLElement | undefined;
  const active = () => !!voice.ask() || voice.permission();
  const cancel = () => { voice.confirm(false); voice.closePermission(); };
  createEffect(() => {
    if (!dialog) return;
    if (!active()) { if (dialog.open) dialog.close(); return; }
    if (!dialog.open) { previousFocus = document.activeElement as HTMLElement; dialog.showModal(); }
    queueMicrotask(() => dialog.querySelector<HTMLElement>("[data-initial]")?.focus());
  });
  createEffect(() => { if (voice.permission()) void bridge.voicePermission().then(setPermission); });
  const title = () => voice.permission() ? tr("Microphone access") : voice.ask()?.kind === "discard" ? tr("Discard voice recording?") : tr("Continue with a cold cache?");
  const body = () => { const ask = voice.ask(); return voice.permission() ? permission()?.text : ask?.kind === "discard" ? tr("Delete the saved, unsent voice recording for %1?", ask.session) : ask?.warning; };
  return <dialog class="dlg" ref={dialog} role="alertdialog" aria-labelledby="voice-dialog-title" style={{ width: "520px" }}
    onCancel={(event) => { event.preventDefault(); cancel(); }} onClose={() => { cancel(); if (previousFocus?.isConnected) previousFocus.focus({ preventScroll: true }); }}>
    <div class="dlg-head"><Icon name={voice.permission() ? "mic" : "triangle-alert"} size={18} class="accent" /><h2 id="voice-dialog-title">{title()}</h2></div>
    <div class="dlg-body"><div class="confirm-body" style={{ "white-space": "pre-wrap" }}>{body()}</div></div>
    <div class="dlg-foot"><span class="spacer" />
      <Show when={voice.permission()} fallback={<>
        <button type="button" class={voice.ask()?.kind === "discard" ? "btn solid-danger" : "btn"} onClick={() => voice.confirm(true)}>{voice.ask()?.kind === "discard" ? tr("Discard recording") : tr("Send with full context")}</button>
        <button type="button" class="btn primary" data-initial onClick={cancel}>{tr("Cancel")}</button>
      </>}>
        <Show when={permission()?.can_open}><button type="button" class="btn" onClick={() => {
          void bridge.voiceOpenSettings().then(() => voice.closePermission(), () => setPermission((old) => ({ can_open: old?.can_open ?? false, text: `${tr("Could not open system settings automatically.")}\n\n${old?.text ?? ""}` })));
        }}>{tr("Open microphone settings")}</button></Show>
        <button type="button" class="btn primary" data-initial onClick={() => voice.closePermission()}>{tr("Close")}</button>
      </Show>
    </div>
  </dialog>;
}
