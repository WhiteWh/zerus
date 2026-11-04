import { nativeRoute } from "./native-entry";
import { tr } from "../../i18n";
import { createActivityScroll } from "./activity-scroll";
import { activityReadingRows, createActivityAnchor } from "./activity-anchor";
import { searchPreview, clearSearchPreview } from "../../store/searchSelection";
import { SearchHighlight } from "./SearchHighlight";
import { For, Show, batch, createContext, createEffect, createMemo, createSignal, on, onCleanup, onMount, untrack, useContext, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";
import { convertFileSrc } from "@tauri-apps/api/core";
import { createImageHandoff, fittedImage } from "./attachment-image";
import { attachmentFetch, attachmentMedia, attachmentOpen, attachmentPin, attachmentUnpin, copyVerbatim, inTauri } from "../../bridge";
import { posterUrl, visiblePoster } from "./video-poster";
import { createVirtualizer, elementScroll, type Range } from "@tanstack/solid-virtual";
import { notify } from "../../store/actions";
import { previews } from "../../store/attachments";
import { mediaPlayback, retainedMediaId, setRetainedMediaId } from "../../store/mediaPlayback";
import { pageHidden } from "../../store/visibility";
import { recordingBlob } from "./voice-playback";
import { cardTarget, createOpen, readCards } from "./attachment-card";
import { createStore, reconcile } from "solid-js/store";
import { Icon } from "../lib/icons";
import { selected, toast, contentScale, subagentOpen, setSubagentOpen, setMenu, setDetailTab, type MenuEntry } from "../../store/ui";
import { activity } from "../../store/session";
import { orderedRows } from "../../store/catalog";
import { flag, num, setSetting } from "../../store/settings";
import type { ActivitySource, AttachmentScope } from "../../store/activitySource";
import { composer, draftKey } from "../../store/composer";
import { terminal } from "../../store/terminal";
import type { LocalMessage } from "../../types";
import { animationFrames, createActivityEnd, newLocalSends } from "./activity-end";
import { replyVisible } from "../../store/viewedReply";
import { buildTranscript, createTranscriptProjection, VOICE_STATE_LABEL, voiceStateDetail, voiceStateIssue } from "./transcript";
import { DELETE_TEXT, DELETE_TITLE, retryReason, routeInspect } from "./local-actions";
import { markdownReady, markdownTree, type Inline, type MarkdownBlock } from "./markdown";
import { activityAll, activityFlow, activityRange, measureActivityRows, activitySelectionIndexes, activitySelectionText, activityText, activityTexts, activityVirtualOffset, selectActivityAll, selectionCoversActivity } from "./activity-virtual";
import { openExternal, openSessionFile } from "./sessionFiles";
import { SessionFileDialog } from "./SessionFileDialog";
import { captionText, statusOf } from "./SessionCard";
import type { ActivityItem, ItemAttachment, LocalItem, MediaMessage, Session, TranscriptItem } from "./types";

// ---------------------------------------------------------------------------
// Reading column (ActivityWidth.h): 900 px at 100% content scale, 480–2400,
// scaled with the content scale, or the full pane. The transcript, queue,
// questions, banners and the composer all share it; Terminal, Processes and
// Native UI ignore it.

export const COLUMN = { min: 480, max: 2400, initial: 900, edge: 10 } as const;
const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
// The width is Settings > Appearance > Activity width (workspace/activityWidth,
// workspace/activityFullWidth): a drag shows it live and stores it once, when
// the pointer lets go. Choosing a skin applies its activity-column variant
// (skin/runtime.ts chooseSkin).
const [dragColumn, setDragColumn] = createSignal<number | undefined>();
const columnWidth = () => dragColumn() ?? clamp(Math.round(num("workspace/activityWidth")), COLUMN.min, COLUMN.max);
const fullWidth = () => flag("workspace/activityFullWidth");
const setFullWidth = (full: boolean) => { if (full !== fullWidth()) void setSetting("workspace/activityFullWidth", full); };
export { columnWidth, fullWidth };

/** Stores a width once, and only when it differs from the stored one. */
function storeColumn(width: number) {
  setDragColumn(undefined);
  const next = clamp(Math.round(width), COLUMN.min, COLUMN.max);
  if (next !== clamp(Math.round(num("workspace/activityWidth")), COLUMN.min, COLUMN.max)) void setSetting("workspace/activityWidth", next);
}
/** Shows a width while Settings' slider moves, or the stored one again (undefined). */
export function previewColumn(width: number | undefined) {
  setDragColumn(width === undefined ? undefined : clamp(Math.round(width), COLUMN.min, COLUMN.max));
}
/** Double-click on an edge or Reset Activity width: the default, centered. */
export function resetColumn() { storeColumn(COLUMN.initial); setFullWidth(false); }

/** CSS variables for everything that shares the column (set on the Activity tab body). */
export const columnStyle = () => ({
  "--content-scale": String(contentScale()),
  "--col": fullWidth() ? "100%" : `${Math.round(columnWidth() * contentScale())}px`,
});

/** Full-width Activity (checkable) and Reset Activity width (disabled at the default). */
export function columnMenuItems(): MenuEntry[] {
  const atDefault = !fullWidth() && columnWidth() === COLUMN.initial;
  return [
    { label: tr("Full-width Activity"), checked: fullWidth(), action: () => setFullWidth(!fullWidth()) },
    { label: tr("Reset Activity width"), icon: "rotate-ccw", disabled: atDefault, action: atDefault ? undefined : resetColumn },
  ];
}

/** A 10 px column edge in the transcript margin; both edges move together. */
function ColumnEdge(props: { right?: boolean }) {
  const [dragging, setDragging] = createSignal(false);
  // A drag cut off (capture lost, the edge unmounted) leaves the stored width.
  const endDrag = () => { if (dragging()) { setDragging(false); setDragColumn(undefined); } };
  onCleanup(endDrag);
  let origin = 0, start = 0, widest = 0, narrowest = 0;
  const measure = (el: HTMLElement) => {
    const scroller = el.closest(".act-scroll") as HTMLElement;
    start = el.parentElement!.getBoundingClientRect().width;
    // Its edges stay within the pane; the width keeps a readable minimum.
    widest = scroller.clientWidth - 2 * COLUMN.edge;
    narrowest = Math.min(widest, COLUMN.min * contentScale());
  };
  const width = (distance: number) => clamp(start + 2 * distance, narrowest, widest) / contentScale();
  return (
    <div class="col-edge" classList={{ right: props.right, dragging: dragging() }} role="separator" aria-orientation="vertical" tabindex="0"
      aria-label={tr("Activity width")} aria-valuemin={COLUMN.min} aria-valuemax={COLUMN.max} aria-valuenow={columnWidth()}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        const el = e.currentTarget;
        el.setPointerCapture(e.pointerId);
        origin = e.clientX;
        measure(el);
        setDragging(true);
      }}
      onPointerMove={(e) => { if (dragging()) setDragColumn(clamp(Math.round(width(props.right ? e.clientX - origin : origin - e.clientX)), COLUMN.min, COLUMN.max)); }}
      onPointerUp={(e) => {
        e.currentTarget.releasePointerCapture(e.pointerId);
        if (!dragging()) return;
        setDragging(false);
        // A click without movement stores nothing.
        const moved = dragColumn();
        if (moved === undefined) return;
        storeColumn(moved);
        setFullWidth(false);
      }}
      onPointerCancel={endDrag}
      onLostPointerCapture={endDrag}
      onDblClick={resetColumn}
      onKeyDown={(e) => {
        const step = e.key === "ArrowLeft" ? -1 : e.key === "ArrowRight" ? 1 : 0;
        if (!step) return;
        e.preventDefault();
        measure(e.currentTarget);
        storeColumn(width((props.right ? step : -step) * 20));
        setFullWidth(false);
      }}
      onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); setMenu({ x: e.clientX, y: e.clientY, items: columnMenuItems() }); }}
    />
  );
}

/** The text browser's menu, then the two width items. */
function activityMenu(e: MouseEvent) {
  if ((e.target as HTMLElement).closest("button, a, input, textarea, .col-edge")) return;
  e.preventDefault();
  const scroller = e.currentTarget as HTMLDivElement;
  const text = activitySelectionText(scroller);
  setMenu({
    x: e.clientX, y: e.clientY, items: [
      { label: tr("Copy"), icon: "copy", kbd: "Ctrl+C", disabled: !text, action: text ? () => copyText(text, tr("The selected text is on the clipboard")) : undefined },
      { label: tr("Select All"), kbd: "Ctrl+A", action: () => selectActivityAll(scroller) },
      { separator: true },
      ...columnMenuItems(),
    ],
  });
}

// ---------------------------------------------------------------------------
// Transcript

const copyText = (text: string, what: string) => {
  void copyVerbatim(text).then(() => toast(tr("Copied"), what), () => toast(tr("Not copied"), tr("The clipboard is unavailable"), "error"));
};

// Inline Markdown stays text except its links: HTTP(S) uses the system
// opener, and local/file references open the session's File reference dialog.

const ActivitySession = createContext<() => Session | undefined>();
const ActivityLayoutHeld = createContext<() => boolean>(() => false);

/** Keep disclosure geometry while a gesture or persistent selection owns
 * layout. In particular, shrinking offscreen content would clamp WebKit's
 * scrollTop even without an application scroll write (zerus-5wmh.85). */
function layoutDisclosure(expanded: () => boolean | undefined = () => undefined, onExpanded?: (open: boolean) => void) {
  const held = useContext(ActivityLayoutHeld);
  const [wanted, setWanted] = createSignal(false);
  const open = createMemo((previous: boolean) => held() ? previous : expanded() ?? wanted(), false);
  const toggle = () => {
    const next = !open();
    setWanted(next);
    onExpanded?.(next);
  };
  return [open, toggle] as const;
}

function inlines(parts: Inline[], session?: () => Session | undefined): JSX.Element[] {
  return parts.map((part): JSX.Element => {
    switch (part.kind) {
      case "text": return part.text;
      case "code": return <code>{part.text}</code>;
      case "br": return <br />;
      case "bold": return <b>{inlines(part.children, session)}</b>;
      case "em": return <em>{inlines(part.children, session)}</em>;
      case "del": return <del>{inlines(part.children, session)}</del>;
      default: {
        const link = part;
        return <a href={link.href} title={link.href} draggable={false} onAuxClick={(event) => event.preventDefault()} onDragStart={(event) => event.preventDefault()} onClick={(event) => {
          event.preventDefault();
          if (link.kind === "external") openExternal(link.href);
          else {
            const current = session?.();
            if (current) openSessionFile(current, link.href);
          }
        }}>{inlines(link.children, session)}</a>;
      }
    }
  });
}

const HEADINGS = ["h1", "h2", "h3", "h4", "h5", "h6"] as const;

function blocks(list: MarkdownBlock[], session?: () => Session | undefined): JSX.Element[] {
  return list.map((b): JSX.Element => {
    switch (b.kind) {
      case "code": return <pre class="act-code mono">{b.text}</pre>;
      case "text": return <>{inlines(b.inline, session)}</>;
      case "heading": return <Dynamic component={HEADINGS[b.level - 1]} class="md-heading">{inlines(b.inline, session)}</Dynamic>;
      case "hr": return <hr class="md-rule" />;
      case "quote": return <blockquote class="md-quote">{blocks(b.blocks, session)}</blockquote>;
      case "list": {
        const items = b.items.map((item) => (
          <li classList={{ "md-task": item.task }}>
            <Show when={item.task}><input type="checkbox" class="md-check" checked={item.checked} disabled aria-label={item.checked ? tr("Done") : tr("Not done")} /></Show>
            {blocks(item.blocks, session)}
          </li>
        ));
        return b.ordered ? <ol class="md-list" start={b.start}>{items}</ol> : <ul class="md-list">{items}</ul>;
      }
      case "table": return (
        <div class="act-table-wrap" role="region" aria-label={tr("Message table")} tabindex="0">
          <table class="act-table" style={{ "--table-columns": b.header.length }}>
            <thead><tr>{b.header.map((cell, column) => <th scope="col" style={{ "text-align": b.align[column] }}>{inlines(cell, session)}</th>)}</tr></thead>
            <Show when={b.rows.length}><tbody>{b.rows.map((row) => <tr>{row.map((cell, column) => <td style={{ "text-align": b.align[column] }}>{inlines(cell, session)}</td>)}</tr>)}</tbody></Show>
          </table>
        </div>
      );
      default: return <p>{inlines(b.inline, session)}</p>;
    }
  });
}

export function Body(props: { text: string }) {
  const session = useContext(ActivitySession);
  // Rerenders once when the lazily loaded parser arrives; Activity places its
  // rows only after that, so placed rows keep their height.
  const tree = createMemo(() => { markdownReady(); return markdownTree(props.text); });
  return <>{blocks(tree(), session)}</>;
}

// ---------------------------------------------------------------------------
// Voice and round-video messages (ActivityView.cpp:978-1025, VoicePlayerImage.h)

const mediaTime = (ms: number) => {
  const s = Math.floor(Math.max(0, ms) / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};

/** Delivery state glyph (18 px); media state never claims reading or completion. */
function DeliveryGlyph(props: { item: MediaMessage }) {
  const s = props.item.state;
  return (
    <span class="vm-state" classList={{ issue: voiceStateIssue(s) }} title={voiceStateDetail(s)} role="img" aria-label={tr(VOICE_STATE_LABEL[s])}>
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <Show when={s === "submitted"} fallback={
          <Show when={s === "cancelled"} fallback={
            <>
              <circle cx="9" cy="9" r="6.5" />
              <Show when={voiceStateIssue(s)} fallback={<path d="M9 5V9L12 10" />}><path d="M9 5V10M9 13H9.01" /></Show>
            </>
          }><path d="M5 5L13 13M13 5L5 13" /></Show>
        }><path d="M3 9L7 13L15 5" /></Show>
      </svg>
    </span>
  );
}

/** 32×32 transcript toggle: a down arrow beside an "A", or a chevron when open. */
function TranscriptButton(props: { open: boolean; onToggle: () => void }) {
  return (
    <button class="vm-tbtn" data-tip={props.open ? tr("Hide transcript") : tr("Show transcript")} aria-label={props.open ? tr("Hide transcript") : tr("Show transcript")}
      aria-expanded={props.open} onClick={props.onToggle}>
      <svg width="32" height="32" viewBox="0 0 32 32" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <Show when={props.open} fallback={<path d="M10 8V19M6 15L10 19L14 15M16 24L20.5 12L25 24M17.5 20H23.5" />}><path d="M10 19L16 13L22 19" /></Show>
      </svg>
    </button>
  );
}

function Waveform(props: { values: number[] }) {
  const bars = () => (props.values.length === 64 ? props.values : []);
  const layer = (cls: string) => (
    <span class={`vp-bars ${cls}`}>
      <Show when={bars().length} fallback={<i class="flat" />}>
        <For each={bars()}>{(v) => <i style={{ height: `${Math.max(2, (27 * v) / 255)}px` }} />}</For>
      </Show>
    </span>
  );
  return <>{layer("rest")}{layer("played")}</>;
}

function MediaCard(props: { item: MediaMessage; scope?: AttachmentScope; sourceId?: string; postersEnabled?: boolean; expanded?: boolean; onExpanded?: (open: boolean) => void }) {
  const it = props.item;
  const video = it.media === "video";
  const [transcript, toggleTranscript] = layoutDisclosure(() => props.expanded, props.onExpanded);
  const held = useContext(ActivityLayoutHeld);
  const [position, setPosition] = createSignal(0);
  const [duration, setDuration] = createSignal(it.durationMs);
  const [isPlaying, setPlaying] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [url, setUrl] = createSignal("");
  const [poster, setPoster] = createSignal("");
  let posterCard: HTMLButtonElement | undefined;
  let posterGate: ReturnType<typeof visiblePoster> | undefined;
  onMount(() => {
    const source = props.sourceId;
    if (!video || !source || !it.posterMessageId || !posterCard || !inTauri()) return;
    const url = posterUrl(source, it.posterMessageId);
    if (url) { posterGate = visiblePoster(posterCard, url, setPoster); posterGate.setEnabled(!!props.postersEnabled); }
  });
  createEffect(() => { const enabled = !!props.postersEnabled; posterGate?.setEnabled(enabled); });
  onCleanup(() => posterGate?.dispose());
  const [error, setError] = createSignal("");
  const visibleError = createMemo((previous: string) => held() ? previous : error() || it.error || "", "");
  const target = createMemo(() => it.file ? cardTarget(it.file, props.scope) : undefined,
    undefined, { equals: (a, b) => JSON.stringify(a) === JSON.stringify(b) });
  let player: HTMLMediaElement | undefined, objectUrl = "";
  let claim: ReturnType<typeof mediaPlayback.take> | undefined;
  let pendingSeek: number | undefined;
  const clear = () => {
    if (claim?.current() && retainedMediaId() === it.id) setRetainedMediaId(undefined);
    claim?.release(); claim = undefined;
    player?.pause(); setPlaying(false); setLoading(false);
    if (player) { player.removeAttribute("src"); player.load(); }
    if (objectUrl) URL.revokeObjectURL(objectUrl);
    objectUrl = ""; pendingSeek = undefined; setUrl(""); setPosition(0);
  };
  createEffect(on(target, () => { clear(); setPosition(0); setDuration(it.durationMs); setError(""); }));
  createEffect(() => { if (pageHidden()) clear(); });
  onCleanup(clear);
  const ratio = () => duration() > 0 ? clamp(position() / duration(), 0, 1) : 0;
  const play = async (seekTo?: number) => {
    const current = target();
    if (!current || loading()) return;
    if (!claim?.current()) claim = mediaPlayback.take(clear, () => { if (loading()) clear(); else player?.pause(); });
    const mine = claim;
    setRetainedMediaId(it.id);
    if (seekTo !== undefined) { pendingSeek = seekTo; setPosition(seekTo); }
    setError("");
    try {
      if (!objectUrl) {
        setLoading(true);
        const bytes = await attachmentMedia(current.computer, current.ref, video);
        if (!mine.current() || target() !== current) return;
        objectUrl = URL.createObjectURL(recordingBlob(bytes, video));
        setUrl(objectUrl);
      }
      await Promise.resolve();
      if (mine.current() && target() === current && player) await player.play();
    } catch (failure) { if (mine.current()) { setError(tr("Could not play recording: %1", String(failure))); setPlaying(false); if (retainedMediaId() === it.id) setRetainedMediaId(undefined); } }
    finally { if (mine.current()) setLoading(false); }
  };
  const pause = () => { player?.pause(); setPlaying(false); };
  const loaded = (event: Event) => {
    if (!claim?.current() || !objectUrl) return;
    const media = event.currentTarget as HTMLMediaElement;
    if (Number.isFinite(media.duration) && media.duration > 0) setDuration(Math.round(media.duration * 1000));
    if (pendingSeek !== undefined) { media.currentTime = Math.min(pendingSeek, duration()) / 1000; pendingSeek = undefined; }
  };
  const moved = (event: Event) => { if (claim?.current() && objectUrl) setPosition(Math.round((event.currentTarget as HTMLMediaElement).currentTime * 1000)); };
  const failed = (event: Event) => {
    if (!claim?.current() || !objectUrl) return;
    setPlaying(false); setLoading(false);
    if (retainedMediaId() === it.id) setRetainedMediaId(undefined);
    setError(tr("Could not play recording: %1", (event.currentTarget as HTMLMediaElement).error?.message || tr("Recording is unavailable.")));
  };
  const seek = (e: MouseEvent) => {
    if (!target() || !duration()) return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const next = Math.round(clamp((e.clientX - r.left) / r.width, 0, 1) * duration());
    if (objectUrl && player && claim?.current()) { player.currentTime = next / 1000; setPosition(next); }
    else void play(next);
  };
  return (
    <article class="act-msg k-user media" id={`item-${it.id}`}>
      <header>
        <b>{video ? tr("You (video)") : tr("You (voice)")}</b>
        <span class="grow" />
        <span class="act-time">{it.at}</span>
        <DeliveryGlyph item={it} />
      </header>
      <Show when={video} fallback={
        <div class="vm-row">
          <div class="vp" style={{ "--p": String(ratio()) }}>
            <button class="vp-btn" aria-label={tr("Play or pause voice message (%1)", mediaTime(duration()))} aria-pressed={isPlaying()}
              disabled={!target() || loading()} title={!target() ? tr("Recording is unavailable.") : undefined} aria-busy={loading()} onClick={() => { if (isPlaying()) pause(); else void play(); }}>
              <svg width="44" height="44" viewBox="0 0 44 44" aria-hidden="true">
                <Show when={isPlaying()} fallback={<path d="M17 12L31 22L17 32Z" />}>
                  <rect x="15" y="13" width="5" height="18" rx="1" /><rect x="24" y="13" width="5" height="18" rx="1" />
                </Show>
              </svg>
            </button>
            <div class="vp-side">
              <div class="vp-wave" role="slider" tabindex="-1" aria-label={tr("Seek")} aria-valuemin={0} aria-valuemax={duration()} aria-valuenow={position()} onClick={seek}>
                <Waveform values={it.waveform} />
              </div>
              <span class="vp-caption tabular">{mediaTime(position())} / {mediaTime(duration())}</span>
            </div>
          </div>
          <Show when={it.transcript}><TranscriptButton open={transcript()} onToggle={toggleTranscript} /></Show>
        </div>
      }>
        {/* The round video plays in place: click plays or pauses; a ring shows progress. */}
        <div class="vm-row video">
          <button ref={posterCard} class="vm-poster" classList={{ playing: isPlaying(), started: position() > 0 && position() < duration() }}
            style={{ "--p": String(ratio()) }} aria-pressed={isPlaying()}
            aria-label={isPlaying() ? tr("Pause video message (%1)", mediaTime(duration())) : tr("Play video message (%1)", mediaTime(duration()))} disabled={!target() || loading()} title={!target() ? tr("Recording is unavailable.") : undefined} aria-busy={loading()} onClick={() => { if (isPlaying()) pause(); else void play(); }}>
            <span class="vm-frames" classList={{ real: !!poster() || isPlaying() }} aria-hidden="true">
              <Show when={poster() && !isPlaying()}><img src={poster()} alt="" /></Show>
              <Show when={url()}><video ref={(el) => { player = el; }} src={url()} playsinline preload="metadata" style={{ position: "absolute", inset: "0", opacity: isPlaying() ? 1 : 0 }}
                onLoadedMetadata={loaded} onTimeUpdate={moved} onPlaying={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} onError={failed} /></Show>
            </span>
            <svg class="vm-ring" viewBox="0 0 200 200" aria-hidden="true">
              <circle class="vm-ring-track" cx="100" cy="100" r="97" pathLength="100" />
              <circle class="vm-ring-done" cx="100" cy="100" r="97" pathLength="100" style={{ "stroke-dashoffset": String(100 - 100 * ratio()) }} />
            </svg>
            <span class="vm-play" aria-hidden="true">
              <svg width="48" height="48" viewBox="0 0 48 48">
                <Show when={isPlaying()} fallback={<path d="M18 12L36 24L18 36Z" />}><rect x="15" y="13" width="6" height="22" rx="1.5" /><rect x="27" y="13" width="6" height="22" rx="1.5" /></Show>
              </svg>
            </span>
            <span class="vm-time tabular">{position() > 0 ? `${mediaTime(position())} / ` : ""}{mediaTime(duration())}</span>
          </button>
          <Show when={it.transcript}><TranscriptButton open={transcript()} onToggle={toggleTranscript} /></Show>
        </div>
      </Show>
      <Show when={transcript() && it.transcript}><p class="vm-text">{it.transcript}</p></Show>
      <Show when={!video}><audio ref={(el) => { player = el; }} src={url() || undefined} preload="none"
        onLoadedMetadata={loaded} onTimeUpdate={moved} onPlaying={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} onError={failed} /></Show>
      <Show when={visibleError()}><p class="vm-error">{visibleError()}</p></Show>
    </article>
  );
}

const openAttachment = createOpen({
  open: (computer, file) => attachmentOpen(computer, file),
  notify: (tone, text, sticky, key) => { notify(tone, text, sticky, key); },
});

/**
 * A message's file as Qt's card shows it (ActivityView.cpp:759-782): an
 * image's thumbnail, the caption, its size and Open →. A file the message
 * saved no reference to only names itself.
 */
function AttachmentCard(props: { file: ItemAttachment; scope?: AttachmentScope }) {
  const target = createMemo(() => cardTarget(props.file, props.scope), undefined, { equals: (a, b) => JSON.stringify(a) === JSON.stringify(b) });
  // An image's thumbnail reads its file while the card shows (ActivityView.cpp:2126-2131).
  createEffect(on(target, (now) => {
    if (!now || !props.file.image) return;
    previews.show(now.computer, now.ref);
    onCleanup(() => previews.hide(now.computer, now.ref));
  }));
  const handoff = createImageHandoff(async (url) => {
    const image = new Image(); image.src = url; await image.decode();
  });
  const localPath = () => props.file.preview_path;
  createEffect(on(localPath, (path) => {
    if (!path) return;
    // Retain the immutable local copy while this row is visible, including
    // after its native local-message record has reconciled away.
    attachmentPin(path);
    handoff.local(convertFileSrc(path, "zerus-file"));
    onCleanup(() => attachmentUnpin(path));
  }));
  const remote = () => { const now=target(); return now && props.file.image ? previews.state(now.computer,now.ref) : undefined; };
  createEffect(() => { const state=remote(); if(state?.kind === "ready") void handoff.remote(state.url); });
  const size = () => props.file.image_size ?? (remote()?.kind === "ready" ? (remote() as Extract<ReturnType<typeof previews.state>,{kind:"ready"}>).fetched.image_size ?? undefined : undefined);
  return <button type="button" class="att-card" aria-disabled={!target()} tabindex={target() ? 0 : -1}
    title={target() ? props.file.caption : tr("Attachment: %1", props.file.name)} onClick={() => { const now=target(); if(now) void openAttachment(now.computer,now.ref); }}>
    <Show when={props.file.image}>
      <span class="att-thumb"><img src={handoff.url()} style={fittedImage(size())} alt=""
        onError={() => { const now=target(); if(now)previews.failed(now.computer,now.ref); }} /></span>
    </Show>
    <span class="att-caption">{props.file.caption}</span>
    <span class="dim">{props.file.meta}</span>
    <span class="att-open" style={{ visibility: target() ? "visible" : "hidden" }}>{tr("Open →")}</span>
  </button>;
}

// ---------------------------------------------------------------------------
// Local message rows: Retry, Check Terminal (or native UI) and Delete
// (ActivityView.cpp:1059-1068, SessionsWindow.cpp messageAction 4470-4527).

/** The row whose Delete waits for its confirmation, and its Delete button, which the focus returns to on Cancel. */
const [deleting, setDeleting] = createSignal<{ id: string; from: HTMLElement }>();

function LocalActions(props: { local: LocalItem }) {
  // Retry stays focusable while off and says why (Rust refuses it while the composer is busy).
  const reason = (action: LocalItem["actions"][number]) => (action === "retry" ? retryReason(composer.view(), composer.operationLocked()) : "");
  const run = (action: LocalItem["actions"][number], from: HTMLElement) => {
    const id = props.local.id;
    if (reason(action)) return;
    if (action === "retry") void composer.localRetry(id);
    else if (action === "delete") setDeleting({ id, from });
    else {
      const showNativeUi = nativeRoute(selected());
      void composer.localInspect(id).then((routed) => routeInspect(routed, {
        showNativeUi,
        showTerminal: () => { terminal.opened(); setDetailTab("terminal"); },
        notify: (tone, text) => { notify(tone, text); },
      }));
    }
  };
  const label = (action: LocalItem["actions"][number]) => (action === "retry" ? tr("Retry") : action === "delete" ? tr("Delete") : props.local.inspectLabel);
  return (
    <div class="act-local-actions">
      <For each={props.local.actions}>{(action) => (
        <button type="button" class="btn small" aria-disabled={reason(action) ? "true" : undefined} title={reason(action) || undefined}
          onClick={(e) => run(action, e.currentTarget)}>{label(action)}</button>
      )}</For>
    </div>
  );
}

/** "Delete message?": Cancel has the focus and is the default; Escape is Cancel. */
function LocalDeleteDialog() {
  let el: HTMLDialogElement | undefined;
  createEffect(() => {
    if (!el) return;
    if (!deleting()) { if (el.open) el.close(); return; }
    if (!el.open) el.showModal();
    queueMicrotask(() => el?.querySelector<HTMLElement>("[data-initial]")?.focus());
  });
  /** Cancel: the focus returns to the row's Delete. */
  const cancel = () => {
    const from = deleting()?.from;
    setDeleting(undefined);
    if (from?.isConnected) from.focus();
  };
  /** Delete: the row goes, and the focus moves to the transcript. */
  const confirm = () => {
    const id = deleting()?.id;
    setDeleting(undefined);
    if (id) void composer.localDelete(id);
    focusTranscript();
  };
  return (
    <dialog class="dlg" ref={el} role="alertdialog" aria-labelledby="local-delete-title" aria-describedby="local-delete-body" style={{ width: "440px" }}
      onCancel={(e) => { e.preventDefault(); cancel(); }} onClose={() => { if (!el?.open && deleting()) cancel(); }}>
      <div class="dlg-head"><Icon name="trash" size={18} class="danger" /><h2 id="local-delete-title">{tr(DELETE_TITLE)}</h2></div>
      <div class="dlg-body"><div class="confirm-body" id="local-delete-body">{tr(DELETE_TEXT)}</div></div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn solid-danger" onClick={confirm}>{tr("Delete")}</button>
        <button type="button" class="btn primary" data-initial onClick={cancel}>{tr("Cancel")}</button>
      </div>
    </dialog>
  );
}

/** The native queue shares the outgoing Markdown/attachment renderer and its
 * guarded file-reference context, without adding sent/delivery actions. */
export function OutgoingMessage(props: { item: ActivityItem; scope?: AttachmentScope; session?: Session }) {
  return <ActivitySession.Provider value={() => props.session}><ItemBody item={props.item} scope={props.scope} /></ActivitySession.Provider>;
}

function ItemBody(props: { item: ActivityItem; scope?: AttachmentScope }) {
  const it = () => props.item;
  return (
    <div class="act-body">
      <Show when={it().kind === "user"} fallback={<Body text={it().body ?? ""} />}>
        {/* A Codex question reply reads as Question and Your answer blocks (ActivityView.cpp:347-363). */}
        <Show when={it().replies?.length} fallback={<Body text={it().body ?? ""} />}>
          <For each={it().replies}>{(r) => (
            <div class="act-reply">
              <div class="act-reply-q">{tr("Question")}</div>
              <p class="wrap-pre">{r.question}</p>
              <div class="act-reply-a"><b>{tr("Your answer")}</b><p class="wrap-pre">{r.answer}</p></div>
            </div>
          )}</For>
        </Show>
      </Show>
      <Show when={it().local?.error}><p class="act-error">{it().local!.error}</p></Show>
      <Show when={it().code}><pre class="act-code mono">{it().code}</pre></Show>
      <Show when={it().attachments}>
        <div class="act-attachments">
          <For each={it().attachments?.map((_, index) => index)}>{(index) => (
            // Reserve the same card geometry before its delivery receipt arrives.
            // A missing request remains read-only; it never authorizes a file read.
            <AttachmentCard file={it().attachments![index]} scope={props.scope} />
          )}</For>
        </div>
      </Show>
      <Show when={it().local?.actions.length}><LocalActions local={it().local!} /></Show>
    </div>
  );
}

function EchoBody(props: { item: ActivityItem; scope?: AttachmentScope }) {
  const request = createMemo(() => {
    if (!props.item.echoBody) return undefined;
    const files = props.item.attachments;
    if (!props.scope || !files?.length) return undefined;
    return { scope: props.scope, files };
  }, undefined, { equals: (a, b) => JSON.stringify(a) === JSON.stringify(b) });
  const [answer, setAnswer] = createSignal<{ current?: { scope: AttachmentScope; files: ItemAttachment[] }; done: boolean; ok: boolean }>({ done: false, ok: false });
  createEffect(on(request, (current) => {
    if (!current) { setAnswer({ current, done: true, ok: false }); return; }
    let live = true;
    setAnswer({ current, done: false, ok: false });
    void readCards(current.files, current.scope, attachmentFetch).then((files) => {
      if (live) setAnswer({ current, done: true, ok: !!files });
    });
    onCleanup(() => { live = false; });
  }));
  const state = () => {
    if (!props.item.echoBody) return "cards";
    const current = request();
    if (props.item.attachments?.length && props.item.attachments.every((file) => file.own)) return "cards";
    if (!current) return "raw";
    const now = answer();
    if (now.current !== current || !now.done) return "loading";
    return now.ok ? "cards" : "raw";
  };
  const shown = createMemo(() => state() === "raw" && props.item.echoBody ? { ...props.item, body: props.item.echoBody, attachments: undefined } : props.item);
  return <ItemBody {...props} item={shown()} />;
}

function Item(props: { item: TranscriptItem; scope?: AttachmentScope; sourceId?: string; postersEnabled?: boolean; expanded?: boolean; onExpanded?: (open: boolean) => void }) {
  const [open, toggleOpen] = layoutDisclosure(() => props.expanded, props.onExpanded);
  const it = props.item;
  if (it.kind === "media") return <MediaCard item={it} scope={props.scope} sourceId={props.sourceId} postersEnabled={props.postersEnabled} expanded={props.expanded} onExpanded={props.onExpanded} />;
  if (it.kind === "context") return <div class="act-label eyebrow" id={`item-${it.id}`}>{it.body}</div>;
  if (it.kind === "thinking") return <div class="act-thinking" id={`item-${it.id}`}><span class="eyebrow">{tr("Thinking")}</span><p>{it.body}</p></div>;
  if (it.kind === "report")
    return (
      <div class="act-notice" id={`item-${it.id}`} classList={{ open: open() }}>
        <div class="row"><Icon name="bot" size={13} /><b>{it.title}</b><Show when={it.sender}><span class="muted">{tr("from %1", it.sender!)}</span></Show><span class="grow" /><span class="act-time">{it.at}</span></div>
        <button class="link" onClick={toggleOpen}>{open() ? tr("▾ Hide report") : tr("▸ Show report")}</button>
        <Show when={open()}><p class="act-notice-body">{it.body}</p></Show>
      </div>
    );
  return (
    <Show when={it.kind !== "tools" && it.kind !== "tool"} fallback={
      <div class="act-tools" id={`item-${it.id}`} classList={{ open: open(), warn: !!it.warn }}>
        <button class="act-tools-head" onClick={toggleOpen} aria-expanded={open()}>
          <Icon name="chevron-right" size={12} class="chev" />
          <b>{it.title}</b>
          <Show when={it.kind === "tools"}><span class="muted">/ {it.body}</span></Show>
          <span class="act-time">{it.at}</span>
        </button>
        <Show when={it.kind === "tool"}><div class="act-tool-cmd mono">{it.body}</div></Show>
        <Show when={open() && it.lines}>
          <div class="act-tool-lines mono"><For each={it.lines}>{(l) => <div>{l}</div>}</For></div>
        </Show>
      </div>
    }>
      <article class={`act-msg k-${it.kind}`} id={`item-${it.id}`}>
        <header>
          <b>{it.label ?? (it.kind === "user" ? tr("You") : it.kind === "agent" ? tr("Agent") : it.sender ?? "Zerus")}</b>
          {/* A local row's stamp stands in for the time and the receipt (ActivityView.cpp:1053-1055). */}
          <Show when={it.local} fallback={<span class="act-time">{it.at}</span>}>
            {(l) => <span class="act-stamp" classList={{ failed: !!l().error || l().uncertain }}>{l().stamp}</span>}
          </Show>
          <Show when={it.kind === "user" && it.receipt}><span class="act-receipt"><Icon name="check" size={11} />{it.receipt}</span></Show>
          <span class="grow" />
          <Show when={it.kind === "agent"}><button class="icon-btn small act-copy" title={tr("Copy as Markdown")} onClick={() => copyText(it.body ?? "", tr("The reply is on the clipboard"))}><Icon name="copy" size={12} /></button></Show>
        </header>
        <EchoBody item={it} scope={props.scope} />
      </article>
    </Show>
  );
}

/** Within this distance of the top, scrolling reads the previous page. */
const OLDER_EDGE = 120;

// The jump to the latest activity (ActivityView.cpp 1607-1636): whether the
// view follows the newest item, how many events arrived since it stopped,
// and the jump back. The composer toolbar shows the jump while the view does
// not follow (Qt's useCompactJumpButton in its Latest slot).
const [following, setFollowing] = createSignal(true);
const [unseen, setUnseen] = createSignal(0);
/** The mounted transcript's scroller (one Activity view shows at a time). */
let liveScroller: HTMLDivElement | undefined;
let liveLatest: (() => void) | undefined;
let liveReply: ((key: string, reply: string) => boolean) | undefined;
export { following, unseen };

/**
 * Whether the mounted Activity shows this reply of the row `key` (Qt's
 * ActivityView::replyVisible): its own live follow only, never a subagent's,
 * an archive's or a search result. Reads geometry only when asked; it never
 * scrolls.
 */
export function activityShowsReply(key: string, reply: string): boolean {
  return !!liveReply?.(key, reply);
}

/**
 * Puts the focus in the transcript (Qt's browser), e.g. before a focused chip
 * hides; false when no transcript is shown to take it.
 */
export function focusTranscript(): boolean {
  if (!liveScroller?.isConnected) return false;
  liveScroller.focus({ preventScroll: true });
  return true;
}

/** Scrolls to the newest item and follows it again. */
export function jumpToLatest() {
  if (searchPreview()) { clearSearchPreview(); return; }
  setFollowing(true);
  setUnseen(0);
  liveLatest?.();
}

/** The transcript items of a source, with the open session's local rows. */
export const transcriptOf = (source: ActivitySource, local: readonly LocalMessage[] = []) => buildTranscript(source.events(), source.transcriptSummary?.() ?? source.summary(), local);

/**
 * Only the matching main or child composer's local rows reach the transcript.
 */
export function localRows(source: ActivitySource, s: Session | undefined): readonly LocalMessage[] {
  const v = composer.view(), child = subagentOpen();
  if (!v || !s) return [];
  if (child) {
    const key = draftKey({ computer: s.computer, key: s.catalogKey, name: s.name, archive_id: null,
      subagent: { agent_id: child, parent_conversation_id: s.conversationId ?? "" } });
    return v.subagent?.agent_id === child && v.key === key ? v.local : [];
  }
  if (source.readOnly) return [];
  const key = draftKey({ computer: s.computer, key: s.catalogKey, name: s.name, archive_id: s.state === "archived" ? s.archiveId ?? null : null });
  return v.key === key ? v.local : [];
}

/** Scrolling near the top reads the previous page, unless one is loading or none is left. */
export const readsOlder = (source: ActivitySource) => !source.loadingOlder() && !source.olderDone() && source.events().length > 0;

/**
 * What the pane shows besides the transcript, for its source and the selected
 * row. A read-only source (an archived row's recorded Activity) shows its
 * transcript and none of the live states; a source that lacks something shows
 * nothing for it.
 */
export function paneStates(source: ActivitySource, s: Session | undefined) {
  const live = source.readOnly || !s || subagentOpen() ? undefined : statusOf(s);
  return {
    /** "This conversation is archived." in place of the transcript. */
    archived: live === "ended",
    working: live === "working",
    attention: live === "attention",
    error: live === "error",
    offline: live === "offline",
    /** "This session left the list; its Activity stopped here." */
    ended: source.ended(),
    /** Why the last read failed, as a notice: read-only sources only (the live pane never showed its follow error). */
    failure: source.readOnly ? source.error() ?? "" : "",
  };
}

export function ActivityView(props: { source?: ActivitySource }) {
  const s = () => selected();
  const preview = () => {
    const p = searchPreview(), row = s();
    return p && row && p.key === row.key && p.run === (row.runId ?? "") && p.conversation === (row.conversationId ?? "") && !subagentOpen() ? p : undefined;
  };
  /** The live follow unless the detail passes another source (an archived row's). */
  const source = (): ActivitySource => props.source ?? activity;
  let scroller!: HTMLDivElement;
  const [placed, setPlaced] = createSignal(false);
  const [layoutHeld, setLayoutHeld] = createSignal(false);
  // Measure new/changed loaded rows while the layout gate is idle. A first
  // estimate correction above the viewport must never move an active reader.
  const [measuring, setMeasuring] = createSignal<number[]>([]);
  const [selectionIndexes, setSelectionIndexes] = createSignal<number[]>([]);
  let measuredContent = new Map<string, string>();
  // Items keep their identity while events append, including across the virtual window.
  const [view, setView] = createStore<{ items: TranscriptItem[]; expired: boolean; loaded: boolean; pane: ReturnType<typeof paneStates>; tool: string }>({ items: [], expired: false, loaded: false, pane: paneStates(source(), s()), tool: "" });
  const local = createMemo(() => localRows(source(), s()), undefined, { equals: (a, b) => a === b || (a.length === 0 && b.length === 0) });
  const projection = createTranscriptProjection();
  let projectedTarget: string | undefined;
  const displayTarget = () => source().displayKey?.() ?? source().target()?.id;
  const built = createMemo(() => {
    const target = displayTarget();
    if (target !== projectedTarget) { projection.reset(); projectedTarget = target; }
    return projection.build(source().events(), source().transcriptSummary?.() ?? source().summary(), local());
  });
  createEffect(() => {
    const items = built(), loaded = source().loaded();
    const expired = source().olderDone() && source().events().length > 0 && source().summary()?.history_truncated === true;
    const pane = paneStates(source(), s());
    const summary = source().summary(), tool = String(summary?.current_tool || summary?.activity_summary || s()?.summary || "");
    resetTarget();
    holdSelection();
    motion.update(() => untrack(() => {
      settleReading(true);
      const content = new Map(items.map((item) => [item.id, JSON.stringify(item)]));
      const pending = new Set(measuring().map((index) => view.items[index]?.id));
      setMeasuring(items.flatMap((item, index) => pending.has(item.id) || content.get(item.id) !== measuredContent.get(item.id) ? [index] : []));
      measuredContent = content;
      for (const key of virtualizer.itemSizeCache.keys()) if (!content.has(String(key))) virtualizer.itemSizeCache.delete(key);
      const disclosures = Object.fromEntries(Object.entries(expanded).filter(([key]) => content.has(key)));
      setExpanded(reconcile(disclosures));
      setView("items", reconcile(items, { key: "id" }));
      setView({ expired, loaded, pane, tool });
      reading.changed(); end.changed();
    }));
  });
  const [expanded, setExpanded] = createStore<Record<string, boolean>>({});
  let olderQueued = false;
  // Keep a named row and its offset while the end pin is released.
  const selectionInside = () => {
    const selection = document.getSelection();
    if (!selection || selection.isCollapsed) return false;
    return [selection.anchorNode, selection.focusNode].some((node) => !!node && scroller.contains(node));
  };
  const playingIndex = createMemo(() => {
    const id = retainedMediaId();
    return id ? view.items.findIndex((item) => item.id === id) : -1;
  });
  const rangeExtractor = createMemo(() => {
    const pending = [...measuring(), ...selectionIndexes()], playing = playingIndex();
    return (range: Range) => activityRange(range, playing, pending);
  });
  const itemKey = createMemo(() => {
    const keys = view.items.map((item) => item.id);
    return (index: number) => keys[index] ?? index;
  });
  const virtualizer = createVirtualizer({
    get count() { return view.items.length; },
    onChange: () => settleReading(),
    scrollToFn: (offset, options, instance) => {
      const write = () => elementScroll(offset, { ...options, behavior: options.behavior ?? "instant" }, instance);
      const top = offset + (options.adjustments ?? 0);
      const element = instance.scrollElement;
      if (!element || top <= element.scrollHeight - element.clientHeight) { write(); return; }
      // Solid commits the new sizer after _willUpdate; defer a write it would clamp.
      const target = source().target()?.id;
      queueMicrotask(() => { if (target === source().target()?.id) write(); });
    },
    getScrollElement: () => (scroller?.isConnected ? scroller : null),
    estimateSize: () => 96,
    measureElement: (element) => element.getBoundingClientRect().height,
    get getItemKey() { return itemKey(); },
    get rangeExtractor() { return rangeExtractor(); },
    overscan: 8,
    gap: 10,
  });
  virtualizer.shouldAdjustScrollPositionOnItemSizeChange = (item, _delta, instance) => {
    if (!end.active() || selectionInside()) return false;
    const offset = (instance.scrollOffset ?? 0) + instance.scrollAdjustments;
    return instance.itemSizeCache.has(item.key)
      ? item.end <= offset && instance.scrollDirection !== "backward"
      : item.start < offset;
  };
  const virtualItems = createMemo(() => virtualizer.getVirtualItems());
  const virtualKeys = createMemo<string[]>(() => virtualItems().map((item) => String(item.key)), [], { equals: (a, b) => a.length === b.length && a.every((key, index) => key === b[index]) });
  // Solid disposes row scopes before removing their DOM. Purge afterwards so
  // the observer cache cannot retain every previously visited row's subtree.
  createEffect(on(virtualKeys, () => { queueMicrotask(() => virtualizer.measureElement(null)); }));
  const byId = createMemo(() => new Map(view.items.map((item) => [item.id, item])));
  const indexOf = createMemo(() => new Map(virtualItems().map((item) => [String(item.key), item.index])));
  const flow = createMemo(() => activityFlow(virtualItems()));

  // Stay at the bottom while it is in view; keep the reading position when an older page lands above.
  /** The newest event's seq the view has seen, for the count of new ones. */
  let newest: number | undefined;
  const end = createActivityEnd(
    () => ({ top: scroller.scrollTop, height: scroller.scrollHeight, viewport: scroller.clientHeight }),
    () => scroller.scrollTo({ top: scroller.scrollHeight, behavior: "instant" }),
    animationFrames,
    () => !motion.active() && !selectionInside(),
    // Wait for final Markdown and the virtual sizer before revealing keyed rows.
    () => {
      if (!markdownReady() || measuring().length || (!view.loaded && !view.items.length) || (view.items.length && !virtualItems().length)) return;
      const target = source().target()?.id;
      requestAnimationFrame(() => {
        if (target === source().target()?.id && !placed() && markdownReady() && !measuring().length && (view.loaded || view.items.length)) setPlaced(true);
      });
    },
  );
  createEffect(on(markdownReady, (ready) => { if (ready) end.changed(); }, { defer: true }));
  createEffect(on(() => virtualItems().length, () => { if (!placed()) end.changed(); }, { defer: true }));
  createEffect(() => {
    const pending = measuring();
    if (!pending.length || !markdownReady()) return;
    const target = source().target()?.id;
    const frame = requestAnimationFrame(() => {
      if (target !== source().target()?.id) return;
      // measureElement deliberately skips sync reads while the virtualizer
      // considers the initial end-placement scroll active. Finish this explicit
      // premeasure before dropping retained rows; otherwise its RO never sees
      // offscreen nodes and estimates change the range on a later gesture.
      measureActivityRows(scroller.querySelectorAll<HTMLElement>(".act-virtual-row"), virtualizer.resizeItem);
      setMeasuring([]);
      end.settle(); reading.changed();
    });
    onCleanup(() => cancelAnimationFrame(frame));
  });
  let latestAfterScroll = false;
  const measuredOffset = (key: string) => {
    const mounted = activityReadingRows(scroller).offset(key);
    if (mounted !== undefined) return mounted;
    const index = view.items.findIndex((item) => `item-${item.id}` === key);
    const start = virtualizer.measurementsCache[index]?.start, virtual = scroller.querySelector<HTMLElement>(".act-virtual");
    if (index < 0 || start === undefined || !virtual) return undefined;
    return activityVirtualOffset(scroller, virtual, start);
  };
  const motion = createActivityScroll(() => scroller.scrollTop, (flush) => {
    const atEnd = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= 1;
    const pinned = end.active();
    reading.remember();
    batch(() => { setLayoutHeld(false); flush(); });
    if ((pinned || atEnd || latestAfterScroll) && !preview()) { latestAfterScroll = false; setFollowing(true); setUnseen(0); reading.reset(); end.pin(); }
    else reading.changed();
  });
  const reading = createActivityAnchor({
    visible: () => activityReadingRows(scroller).visible(),
    offset: measuredOffset,
    adjust: (delta) => activityReadingRows(scroller).adjust(delta),
    scrollTop: () => scroller.scrollTop,
  }, () => !end.active() && !preview(), animationFrames, () => !motion.active() && !selectionInside());
  const holdSelection = () => {
    if (selectionInside()) { setLayoutHeld(true); motion.selection(true); }
  };
  const settleReading = (before = false) => { if (before) reading.beforeChange(); else reading.changed(); };
  const scrollIntent = () => {
    if (!placed()) return; // Initial placement owns the hidden loading layout.
    setLayoutHeld(true); motion.intent(); latestAfterScroll = false;
    end.release(); setFollowing(false); reading.remember();
  };
  const latest = () => {
    holdSelection();
    if (motion.active()) { latestAfterScroll = true; return; }
    reading.reset(); end.pin();
    scroller.scrollTo({ top: scroller.scrollHeight, behavior: "instant" });
  };
  const showsReply = (key: string, reply: string) => {
    const shown = source();
    if (shown.readOnly || subagentOpen() || shown.target()?.id !== key || !scroller.isConnected) return false;
    const rows = activityReadingRows(scroller);
    return replyVisible({
      settled: () => placed() && !preview() && !motion.deferred(),
      below: () => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight,
      viewport: () => scroller.clientHeight,
      offset: (id) => rows.offset(id),
    }, reply, shown.events(), shown.summary());
  };
  onMount(() => {
    liveScroller = scroller;
    liveLatest = latest;
    liveReply = showsReply;
    const pointerEnded = () => motion.release();
    window.addEventListener("pointerup", pointerEnded); window.addEventListener("pointercancel", pointerEnded); window.addEventListener("blur", pointerEnded);
    onCleanup(() => { window.removeEventListener("pointerup", pointerEnded); window.removeEventListener("pointercancel", pointerEnded); window.removeEventListener("blur", pointerEnded); });
    activityTexts.set(scroller, () => activityText(view.items, expanded));
    const selectionChanged = () => {
      if (!selectionCoversActivity(scroller)) activityAll.delete(scroller);
      const inside = selectionInside();
      setSelectionIndexes(inside ? activitySelectionIndexes(scroller, view.items.length) : []);
      if (inside) setLayoutHeld(true);
      motion.selection(inside);
    };
    document.addEventListener("selectionchange", selectionChanged);
    // Content includes Markdown, images and tool disclosures; the viewport
    // also resizes when the composer or a question card changes height.
    const changed = () => { end.changed(); settleReading(); };
    // Resizes settle before the frame paints, so opening a session never shows
    // its content shifted by a late card, image or Markdown height for a frame.
    let measuredWidth = scroller.clientWidth;
    const resize = new ResizeObserver(() => {
      if (scroller.clientWidth !== measuredWidth) {
        measuredWidth = scroller.clientWidth;
        setMeasuring(view.items.map((_, index) => index));
      }
      end.settle(); settleReading();
    });
    resize.observe(scroller);
    const column = scroller.querySelector(".act-col");
    const mutations = new MutationObserver(changed);
    if (column) {
      resize.observe(column);
      mutations.observe(column, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["class", "style", "src", "width", "height", "hidden", "open"] });
    }
    scroller.addEventListener("load", changed, true);
    scroller.addEventListener("contentvisibilityautostatechange", changed, true);
    const fontChanged = () => {
      setMeasuring(view.items.map((_, index) => index));
      changed();
    };
    document.fonts?.addEventListener("loadingdone", fontChanged);
    onCleanup(() => { document.removeEventListener("selectionchange", selectionChanged); resize.disconnect(); mutations.disconnect(); scroller.removeEventListener("load", changed, true); scroller.removeEventListener("contentvisibilityautostatechange", changed, true); document.fonts?.removeEventListener("loadingdone", fontChanged); });
  });
  onCleanup(() => {
    end.dispose(); reading.dispose(); motion.dispose();
    activityTexts.delete(scroller);
    activityAll.delete(scroller);
    if (liveScroller === scroller) liveScroller = undefined;
    if (liveLatest === latest) liveLatest = undefined;
    if (liveReply === showsReply) liveReply = undefined;
  });
  const onScroll = () => {
    if (motion.active()) {
      motion.moved(); reading.remember();
      if (!olderQueued && !preview() && scroller.scrollTop < OLDER_EDGE && readsOlder(source())) {
        const target = source().target()?.id;
        olderQueued = true;
        queueMicrotask(() => {
          olderQueued = false;
          if (target === source().target()?.id && scroller.scrollTop < OLDER_EDGE && readsOlder(source())) source().loadOlder();
        });
      }
      return;
    }
    if (end.scrolled()) return;
    reading.changed();
  };
  let sendIds: Set<string> | undefined;
  let layoutTarget: string | undefined | null = null;
  const resetTarget = () => {
    const target = displayTarget();
    if (layoutTarget === target) return;
    layoutTarget = target;
    // An explicit session switch ends its old selection. Do this before the
    // first new projection, so a held range cannot defer a cached first page.
    if (selectionInside()) document.getSelection()?.removeAllRanges();
    sendIds = undefined; setPlaced(false);
    motion.reset(); setLayoutHeld(false); latestAfterScroll = false;
    activityAll.delete(scroller);
    setExpanded(reconcile({}));
    measuredContent.clear(); setMeasuring([]); setSelectionIndexes([]);
    virtualizer.measure();
    setFollowing(true); setUnseen(0); reading.reset(); newest = undefined;
    // Selection changes must place even when the next page has the same row count.
    end.pin();
  };
  createEffect(on(displayTarget, resetTarget));
  createEffect(on(local, (rows) => {
    const sends = newLocalSends(sendIds, rows.map((row) => row.id));
    sendIds = sends.baseline;
    if (sends.added.length) jumpToLatest();
  }));
  createEffect(on(preview, (match) => {
    if (match) {
      setPlaced(true);
      motion.reset(); setLayoutHeld(false); end.release(); reading.reset(); setFollowing(false);
      requestAnimationFrame(() => { if (!motion.active() && !selectionInside()) scroller.querySelector(".search-preview mark")?.scrollIntoView({ block: "center" }); });
    } else { setFollowing(true); end.pin(); }
  }));
  // Events after the newest one seen count as new while the view does not follow (Qt's m_unseen).
  createEffect(on(() => source().events(), (events) => {
    const last = events.at(-1)?.seq;
    if (newest !== undefined && last !== undefined && last > newest && !following()) {
      let added = 0;
      for (let i = events.length - 1; i >= 0 && (events[i].seq ?? -Infinity) > newest; i--) added++;
      setUnseen((n) => n + added);
    }
    if (last !== undefined) newest = last;
  }));

  const scope = createMemo(() => source().attachmentScope?.(), undefined, { equals: (a, b) => JSON.stringify(a) === JSON.stringify(b) });
  const liveTool = () => view.tool;
  const states = () => view.pane;
  const fileSession = () => {
    const id = source().target()?.id;
    return orderedRows().find((row) => row.key === id);
  };
  return (
    <ActivitySession.Provider value={fileSession}>
    <ActivityLayoutHeld.Provider value={layoutHeld}>
    <div class="activity" style={{ position: "relative" }}>
      <Show when={!placed()}>
        <div class="act-col" style={{ position: "absolute", inset: "0", "min-height": "0", "pointer-events": "none" }} role="status">
          <div class="act-label eyebrow">{tr("Recorded activity — messages and tool excerpts")}</div>
          <div class="act-older hint">{tr("Reading activity…")}</div>
        </div>
      </Show>
      <LocalDeleteDialog />
      <SessionFileDialog />
      <div class="act-scroll" ref={scroller} tabindex="-1" onContextMenu={activityMenu} onScroll={onScroll} style={{ "overflow-anchor": "none" }}
        onWheel={scrollIntent}
        onKeyDown={(e) => {
          if (e.defaultPrevented || (e.target as HTMLElement).closest("input,textarea,select,[contenteditable=true]")) return;
          const key = e.key.toLowerCase(), command = (e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey;
          if (command && key === "a") { e.preventDefault(); selectActivityAll(scroller); return; }
          if (command && key === "c" && activityAll.get(scroller) && selectionCoversActivity(scroller)) {
            e.preventDefault();
            const text = activitySelectionText(scroller);
            if (text) copyText(text, tr("The selected text is on the clipboard"));
            return;
          }
          if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(e.key)
            && !(e.key === " " && (e.target as HTMLElement).closest("button,a"))) scrollIntent();
        }}
        onPointerDown={(e) => { if (e.button === 0 && !(e.target as HTMLElement).closest("button,a,input,textarea,.col-edge")) {
          // The scrollbar releases follow. Selecting text merely pauses it;
          // the persistent range keeps ownership after pointerup.
          if (e.target === scroller) scrollIntent();
          else setLayoutHeld(true);
          motion.hold();
        } }}
        onTouchStart={() => { scrollIntent(); motion.hold(); }} onTouchMove={scrollIntent} onTouchEnd={() => motion.release()} onTouchCancel={() => motion.release()}>
        <div class="act-col" style={{ visibility: placed() ? "visible" : "hidden" }}>
          <Show when={!fullWidth()}><ColumnEdge /><ColumnEdge right /></Show>
          <Show when={subagentOpen()}>
            <div class="subagent-bar">
              <button class="btn small" onClick={() => setSubagentOpen(undefined)}><Icon name="arrow-right" size={12} style={{ rotate: "180deg" }} />{tr("Main activity")}</button>
              <b>{s()?.subagents?.find((k) => k.id === subagentOpen())?.name}</b>
              <span class="hint">{composer.view()?.subagent?.hint ?? tr("Loading subagent activity…")}</span>
            </div>
          </Show>
          <Show when={preview()}>{(p) => <section class="search-preview" aria-label={tr("Search result")}>
            <div class="act-label eyebrow">{tr("SEARCH RESULT — Saved message")}</div>
            <Show when={p().hit.content_truncated}><div class="hint">{tr("Excerpt around the match. This message is too long to display in full.")}</div></Show>
            <div class="search-preview-body"><SearchHighlight text={String(p().hit.event.detail ?? "")} query={p().query} /></div>
          </section>}</Show>
          <Show when={!preview()}>
          <div class="act-history-header">
          <div class="act-label eyebrow" style={{ position: "relative", flex: "1", "min-width": "0" }}>
            <span style={{ visibility: source().loadingOlder() ? "hidden" : "visible" }}>{tr("Recorded activity — messages and tool excerpts")}</span>
            <span class="act-older hint" style={{ position: "absolute", inset: "0", padding: "0", display: "flex", "align-items": "center", "justify-content": "center", "text-transform": "none", "letter-spacing": "normal", "font-weight": "normal", visibility: source().loadingOlder() ? "visible" : "hidden" }} aria-hidden={!source().loadingOlder()}>{tr("Reading earlier activity…")}</span>
          </div>
          <Show when={!source().olderDone() && source().events().length > 0}>
            <button type="button" class="btn small ghost act-read-older" disabled={source().loadingOlder()} onClick={() => source().loadOlder()}>{tr("Read earlier activity")}</button>
          </Show>
          </div>
          <Show when={view.expired}>
            <div class="act-older hint">{tr("Older events have expired from the journal.")}</div>
          </Show>
          <Show when={!states().archived} fallback={<div class="notice"><Icon name="archive" size={14} /><div>{tr("This conversation is archived.")}</div></div>}>
            <Show when={view.loaded || view.items.length} fallback={<div class="act-older hint">{tr("Reading activity…")}</div>}>
              <Show when={view.items.length} fallback={<div class="act-older hint">{tr("No recorded activity yet.")}</div>}>
                {/* Reserve the current virtual extent through range swaps. Without
                    it, a synchronous layout between removing rows and updating
                    spacers makes WebKit clamp scrollTop even during a gesture.
                    A minimum still lets late, unmeasured content grow naturally. */}
                <div class="act-virtual" style={{ "min-height": `${virtualizer.getTotalSize()}px` }}>
                  <div class="act-virtual-spacer" style={{ height: `${flow().padding}px` }} />
                  <div class="act-virtual-flow">
                  <For each={virtualKeys()}>{(id) => {
                    const item = () => byId().get(id);
                    // Solid writes data-index after this ref, so supply it before measuring.
                    return (
                      <div class="act-virtual-row" data-index={indexOf().get(id) ?? 0} data-start={flow().starts.get(id) ?? 0}
                        ref={(node) => { node.dataset.index = String(indexOf().get(id) ?? 0); virtualizer.measureElement(node); }}
                        style={{ "margin-top": `${flow().margins.get(id) ?? 0}px` }}>
                        <Show when={item()}>{(it) => <Item item={it()} scope={scope()} sourceId={source().target()?.id} postersEnabled={placed() && !pageHidden()} expanded={!!expanded[id]} onExpanded={(open) => setExpanded(id, open)} />}</Show>
                      </div>
                    );
                  }}</For>
                  </div>
                  <div class="act-virtual-spacer" style={{ height: `${Math.max(0, virtualizer.getTotalSize() - (virtualItems().at(-1)?.end ?? 0))}px` }} />
                </div>
              </Show>
            </Show>
            <Show when={states().working && liveTool()}>
              <div class="act-live"><span class="typing"><i /><i /><i /></span>{tr("Agent is running")} <b class="mono ell">{liveTool()}</b></div>
            </Show>
            <Show when={states().attention}>
              <div class="notice warn"><Icon name="circle-alert" size={14} /><div><b>{captionText(s()?.caption ?? "")}.</b> {tr("Answer it in the agent's terminal or in the Zerus app; answering here arrives with the session actions.")}</div></div>
            </Show>
            <Show when={states().error}>
              <div class="notice danger"><Icon name="circle-alert" size={14} /><div><b>{captionText(s()?.caption ?? "")}.</b> {s()?.summary}</div></div>
            </Show>
            <Show when={states().offline}>
              <div class="notice warn"><Icon name="wifi-off" size={14} /><div><b>{tr("%1 is offline.", s()?.machine ?? "")}</b> {tr("This is the last activity received. It updates again when the machine reconnects.")}</div></div>
            </Show>
            <Show when={states().ended}>
              <div class="notice"><Icon name="archive" size={14} /><div>{tr("This session left the list; its Activity stopped here.")}</div></div>
            </Show>
            <Show when={states().failure}>
              <div class="notice danger"><Icon name="circle-alert" size={14} /><div>{states().failure}</div></div>
            </Show>
          </Show>
          </Show>
        </div>
      </div>
    </div>
    </ActivityLayoutHeld.Provider>
    </ActivitySession.Provider>
  );
}
