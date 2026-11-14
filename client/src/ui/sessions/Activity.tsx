import { nativeRoute } from "./native-entry";
import { withoutHeld } from "./held-message";
import { tr, trNoop } from "../../i18n";
import { createActivityScroll } from "./activity-scroll";
import { activityReadingRows, createActivityAnchor } from "./activity-anchor";
import { activityAnchorSeq, activityNearestRow, activityPositionCursor, activityPositions, type ActivityPosition } from "./activity-position";
import { ActivityHeights, activityMeasureWindow, activityOverscan, createActivityResize, syncActivityHeights } from "./activity-heights";
import { searchPreview, clearSearchPreview } from "../../store/searchSelection";
import { SearchHighlight } from "./SearchHighlight";
import { For, Show, batch, createContext, createEffect, createMemo, createSignal, on, onCleanup, onMount, untrack, useContext, type JSX } from "solid-js";
import { convertFileSrc } from "@tauri-apps/api/core";
import { createImageHandoff, fittedImage } from "./attachment-image";
import { attachmentFetch, attachmentMedia, attachmentOpen, attachmentPin, attachmentUnpin, copyVerbatim, inTauri } from "../../bridge";
import { posterUrl, visiblePoster } from "./video-poster";
import { createVirtualizer, elementScroll, type Range } from "@tanstack/solid-virtual";
import { followedFor, notify } from "../../store/actions";
import { previews } from "../../store/attachments";
import { mediaPlayback, retainedMediaId, setRetainedMediaId } from "../../store/mediaPlayback";
import { pageHidden } from "../../store/visibility";
import { recordingBlob } from "./voice-playback";
import { cardTarget, createOpen, readCards, rowScope } from "./attachment-card";
import { createStore, reconcile } from "solid-js/store";
import { Icon } from "../lib/icons";
import { ToolRow, TOOL_OUTPUT, outputKey, type ProcessLinks } from "./ToolEvents";
import { processes, processViewKey, recordedProcess } from "../../store/processes";
import { selected, toast, contentScale, subagentOpen, setSubagentOpen, setMenu, setDetailTab, takeLatestRequest, type MenuEntry } from "../../store/ui";
import { activity, processesTarget } from "../../store/session";
import { computerOffline, orderedRows } from "../../store/catalog";
import { flag, num, setSetting } from "../../store/settings";
import type { ActivitySource, AttachmentScope } from "../../store/activitySource";
import { composer, draftKey } from "../../store/composer";
import { terminal } from "../../store/terminal";
import type { ActivityEvent, LocalMessage } from "../../types";
import { animationFrames, createActivityEnd, newLocalSends } from "./activity-end";
import { replyVisible } from "../../store/viewedReply";
import { buildTranscript, createActivityProjections, VOICE_STATE_LABEL, voiceStateDetail, voiceStateIssue } from "./transcript";
import { DELETE_TEXT, DELETE_TITLE, retryReason, routeInspect } from "./local-actions";
import { NOT_DELIVERED_HINT } from "./inbox-actions";
import { createInboxRemoveScope, InboxRemove, RemoveQueuedDialog, type InboxRemovePorts, type InboxRemoveScope } from "./InboxRemove";
import { emptyActivity } from "./activity-empty";
import { refreshRouteActivity, routeActivityOf, routeSessionOf } from "../../store/questionRouteActivity";
import { markdownReady, markdownTree } from "./markdown";
import { ActivitySession, Body, HighlightQuery } from "./MarkdownBody";
import { searchHitItems } from "./search-hit";
import { ActivityPending } from "./ActivityPending";

export { Body } from "./MarkdownBody";
import { activityAll, activityFlow, activityRange, measureActivityRows, activitySelectionIndexes, activitySelectionText, activityText, activityTexts, activityVirtualOffset, selectActivityAll, selectionCoversActivity } from "./activity-virtual";
import { captionText, statusOf } from "./SessionCard";
import { formatElapsed, workingLine } from "./inspector-data";
import { foldedBody, noticeBar, type SystemNotice } from "./notices";
import { NOTICE_MODES, foldNoticeTurns, sessionNoticeMode, withSessionNoticeMode, type NoticeMode } from "./leadNotices";
import { teams, sessionIs } from "../../store/teams";
import { settings } from "../../store/settings";
import { MediaStrip, MediaViewerDialog, VideoMessageDialog } from "./MediaViewer";
import { mediaReferences } from "./media";
import { onMediaNotice, type MediaScope } from "../../store/media";
import type { ActivityItem, ItemAttachment, LocalItem, MediaMessage, Session, TranscriptItem } from "./types";
import { lazyView } from "../lazy-view";
import { LazyBoundary } from "../lazy-boundary";
// The File reference dialog opens on demand, outside the entry chunk (zerus-ycycu).
const SessionFileDialog = lazyView(() => import("./SessionFileDialog"), (m) => m.SessionFileDialog);

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

// Task 248: what Qt says in its notice line when a media file cannot be opened or loads from a peer.
onMediaNotice((text) => toast(tr("Media"), text, "info", false));

/** Task 248: where the Activity's session resolves the media its agent names. */
function useMediaScope(): () => MediaScope | undefined {
  const session = useContext(ActivitySession);
  return () => {
    const current = session?.();
    return current ? { sessionKey: current.key, computer: current.computer, cwd: current.cwdPath || current.cwdCanonical || "", machine: current.machine } : undefined;
  };
}

/** The pictures, animations and videos a reply names, previewed under it (Qt mediaTiles). */
function ReplyMedia(props: { text: string }) {
  const scope = useMediaScope();
  const refs = createMemo(() => { markdownReady(); return mediaReferences(markdownTree(props.text)); }, [], { equals: (a, b) => a.join("\n") === b.join("\n") });
  return <MediaStrip scope={scope()} refs={refs()} />;
}

/** The pictures a tool group worked with (Qt toolMedia), under the group. */
function ToolMedia(props: { refs: readonly string[] }) {
  const scope = useMediaScope();
  return <MediaStrip scope={scope()} refs={props.refs} />;
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
  // Task 248: Qt plays a round video message in its own "Video message" dialog (ActivityView::playVoice).
  const [dialogUrl, setDialogUrl] = createSignal("");
  const openDialog = async () => {
    const current = target();
    if (!current || loading()) return;
    pause();
    if (!objectUrl) {
      setLoading(true);
      try {
        const bytes = await attachmentMedia(current.computer, current.ref, true);
        if (target() !== current) return;
        objectUrl = URL.createObjectURL(recordingBlob(bytes, true));
        setUrl(objectUrl);
      } catch (failure) { setError(tr("Could not play recording: %1", String(failure))); return; }
      finally { setLoading(false); }
    }
    setDialogUrl(objectUrl);
  };
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
    <article class="act-msg k-user media" id={`item-${it.id}`} classList={{ inherited: !!it.inherited }}>
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
          <button class="icon-btn small vm-open" aria-label={tr("Video message")} title={tr("Video message")} data-video-dialog disabled={!target() || loading()} onClick={() => void openDialog()}><Icon name="maximize-2" size={14} /></button>
          <Show when={it.transcript}><TranscriptButton open={transcript()} onToggle={toggleTranscript} /></Show>
        </div>
        <Show when={dialogUrl()}><VideoMessageDialog url={dialogUrl()} onClose={() => setDialogUrl("")} /></Show>
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
    {/* "inherit", never "visible": a row inside the column that is still
        hidden while it is placed must stay hidden (zerus-ig5q). */}
    <span class="att-open" style={{ visibility: target() ? "inherit" : "hidden" }}>{tr("Open →")}</span>
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

/** The open Activity's Remove (zerus-5i2e5): its review lives and ends with that Activity. */
const InboxScope = createContext<InboxRemoveScope>();

/** Remove for a not-delivered OpenCode message: the composer's view and its Rust command (zerus-5wmh.13.6). */
const inboxPorts: InboxRemovePorts = {
  view: () => composer.view(),
  remove: (handle, run, conversation, messageId) => composer.inboxRemove(handle, run, conversation, messageId),
  notify: (tone, text, sticky) => { notify(tone, text, sticky); },
  done: () => { focusTranscript(); },
};

/** The native queue shares the outgoing Markdown/attachment renderer and its
 * guarded file-reference context, without adding sent/delivery actions. */
export function OutgoingMessage(props: { item: ActivityItem; scope?: AttachmentScope; session?: Session }) {
  return <ActivitySession.Provider value={() => props.session}><ItemBody item={props.item} scope={props.scope} /></ActivitySession.Provider>;
}

function ItemBody(props: { item: ActivityItem; scope?: AttachmentScope }) {
  const it = () => props.item;
  const inboxScope = useContext(InboxScope);
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
      {/* Not delivered is not a refusal: OpenCode keeps the message and runs it first; Zerus never resends it. */}
      <Show when={it().inbox?.state === "not_delivered"}><p class="act-inbox-hint">{tr(NOT_DELIVERED_HINT)}</p></Show>
      <Show when={inboxScope && it().inbox?.removable ? it().inbox : undefined}>{(item) => <InboxRemove item={item()} scope={inboxScope!} />}</Show>
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

/** Qt's team plaque (ActivityView.cpp teamMessage): kind, sender and receiver,
 * task and attempt, three lines of the body, then the header Zerus pasted. */
function NoticeCard(props: { item: ActivityItem; notice: SystemNotice; open: boolean; toggle: () => void }) {
  const n = () => props.notice;
  const folded = createMemo(() => foldedBody(n()));
  const bar = () => noticeBar(n().from);
  return (
    <article class="act-msg k-system act-plaque" id={`item-${props.item.id}`} classList={{ inherited: !!props.item.inherited }}
      style={{ "--bar-dark": bar().dark, "--bar-light": bar().light }} data-notice={n().from}>
      <header>
        <b class="act-plaque-kind">{n().kind}</b>
        <span class="act-plaque-chip">{n().to ? `${n().from} → ${n().to}` : n().from}</span>
        <Show when={n().task}><span class="act-plaque-chip task">{n().title ? tr("Task: %1", n().title) : tr("Task #%1", n().task)}</span></Show>
        <Show when={n().attempt}><span class="act-plaque-chip">{tr("Attempt %1", n().attempt)}</span></Show>
        <span class="grow" />
        <span class="act-time">{props.item.at}</span>
      </header>
      <p class="act-plaque-body wrap-pre">{props.open ? n().full : folded().text}</p>
      <footer class="act-plaque-foot">
        <span class="mono">{n().header}</span><span class="grow" />
        <Show when={folded().clipped || props.open}><button class="link" onClick={props.toggle}>{props.open ? tr("Show less") : tr("Show more")}</button></Show>
      </footer>
    </article>
  );
}

/** Task 244: a folded team notice or the lead's folded reply to one, as Qt's tool-call row. */
function FoldedItem(props: { item: ActivityItem; open: boolean; toggle: () => void; scope?: AttachmentScope; sourceId?: string }) {
  const it = props.item;
  return (
    <div class="act-tools act-folded" id={`item-${it.id}`} classList={{ open: props.open, reply: it.foldKind === "reply" }} data-folded={it.foldKind}>
      <button class="act-tools-head" onClick={props.toggle} aria-expanded={props.open}>
        <Icon name="chevron-right" size={12} class="chev" />
        <b>{it.sender}</b>
        <span class="muted ell">{it.title}</span>
        <span class="act-time">{it.at}</span>
      </button>
      <Show when={props.open}>
        <div class="act-folded-body"><For each={it.folded ?? []}>{(child) => <Item item={child} scope={props.scope} sourceId={props.sourceId} />}</For></div>
      </Show>
    </div>
  );
}

/** Task 244: whether `row` is its team's lead session (the modes apply there only). */
export function isLeadSession(row: Session | undefined): boolean {
  if (!row) return false;
  const team = teams.teamOf(row);
  return !!team?.members.some((member) => member.lead && sessionIs(member.session, row.computer, row));
}

/** Task 244: the lead session's mode for team notices (its own choice, else Settings > Teams). */
export const leadNoticeMode = (row: Session): NoticeMode => sessionNoticeMode(settings["teams/leadNoticeSessions"], row.key, settings["teams/leadNotices"]);
export function setLeadNoticeMode(row: Session, mode: NoticeMode) {
  void setSetting("teams/leadNoticeSessions", withSessionNoticeMode(settings["teams/leadNoticeSessions"], row.key, mode));
}
const NOTICE_LABEL: Record<NoticeMode, string> = { full: trNoop("Full"), collapsed: trNoop("Collapsed"), silent: trNoop("Hidden") };

/**
 * A submitted question route's line (zerus-sa9qn.33): who it went to or came
 * from, the child's preview as plain text and its reference. A parent's own
 * message of exactly the routed text folds in behind the disclosure.
 */
function RouteReceipt(props: { item: ActivityItem; open: boolean; onToggle: () => void }) {
  const route = () => props.item.route!;
  return (
    <article class="act-msg k-receipt act-route" id={`item-${props.item.id}`}>
      <header><b>{props.item.sender}</b><span class="act-time">{props.item.at}</span><span class="grow" /></header>
      <div class="act-body">
        <p class="act-route-line"><span class="mono">{route().role === "child" ? route().parent : route().child}</span>
          <Show when={route().preview}><q class="act-route-preview">{route().preview}</q></Show></p>
        <div class="row act-route-ref">
          <span class="mono ell" title={route().reference}>{route().reference}</span>
          <button class="icon-btn small" title={tr("Copy session reference")} onClick={() => copyText(route().reference, tr("The session reference is on the clipboard"))}><Icon name="copy" size={12} /></button>
        </div>
        <Show when={route().text}>
          <button class="link" onClick={() => props.onToggle()}>{props.open ? tr("▾ Hide the received message") : tr("▸ Show the received message")}</button>
          <Show when={props.open}><p class="wrap-pre act-route-text">{route().text}</p></Show>
        </Show>
      </div>
    </article>
  );
}

function Item(props: {
  item: TranscriptItem; scope?: AttachmentScope; sourceId?: string; postersEnabled?: boolean; expanded?: boolean; onExpanded?: (open: boolean) => void;
  /** A tool row's Show more state per event, and View process links. */
  shown?: (index: number) => number | undefined; onMore?: (index: number) => void; process?: ProcessLinks;
}) {
  const [open, toggleOpen] = layoutDisclosure(() => props.expanded, props.onExpanded);
  const it = props.item;
  if (it.kind === "folded") return <FoldedItem item={it} open={open()} toggle={toggleOpen} scope={props.scope} sourceId={props.sourceId} />;
  if (it.kind === "system" && it.notice) return <NoticeCard item={it} notice={it.notice} open={open()} toggle={toggleOpen} />;
  if (it.kind === "media") return <MediaCard item={it} scope={rowScope(it, props.scope)} sourceId={props.sourceId} postersEnabled={props.postersEnabled} expanded={props.expanded} onExpanded={props.onExpanded} />;
  // A context line with a time shows it apart, never after a dot.
  if (it.kind === "context") return <div class="act-label eyebrow" id={`item-${it.id}`}>{it.body}<Show when={it.at}><span class="act-time"> {it.at}</span></Show></div>;
  if (it.kind === "receipt" && it.route) return <RouteReceipt item={it} open={open()} onToggle={toggleOpen} />;
  // Thinking and subagent reports are Markdown, as in Qt (ActivityView.cpp:942-975).
  if (it.kind === "thinking") return <div class="act-thinking" id={`item-${it.id}`} classList={{ inherited: !!it.inherited }}><span class="eyebrow">{tr("Thinking")}</span><div class="act-thinking-body act-body"><Body text={it.body ?? ""} /></div></div>;
  if (it.kind === "report")
    return (
      <div class="act-notice" id={`item-${it.id}`} classList={{ open: open(), inherited: !!it.inherited }}>
        <div class="row"><Icon name="bot" size={13} /><b>{it.title}</b><Show when={it.sender}><span class="muted">{tr("from %1", it.sender!)}</span></Show><span class="grow" /><span class="act-time">{it.at}</span></div>
        <button class="link" onClick={toggleOpen}>{open() ? tr("▾ Hide report") : tr("▸ Show report")}</button>
        <Show when={open()}><div class="act-notice-body act-body"><Body text={it.body ?? ""} /></div></Show>
      </div>
    );
  return (
    <Show when={it.kind !== "tools" && it.kind !== "tool" && it.kind !== "warm"} fallback={
      <>
        <ToolRow item={it} open={open()} onToggle={toggleOpen} shown={(index) => props.shown?.(index)}
          onMore={(index) => props.onMore?.(index)} process={props.process} />
        <Show when={it.media?.length}><ToolMedia refs={it.media!} /></Show>
      </>
    }>
      <article class={`act-msg k-${it.kind}`} id={`item-${it.id}`} classList={{ inherited: !!it.inherited }}
        style={it.kind === "system" ? { "--bar-dark": noticeBar(it.sender ?? "").dark, "--bar-light": noticeBar(it.sender ?? "").light } : undefined}>
        <header>
          <b>{it.label ?? (it.kind === "user" ? tr("You") : it.kind === "agent" ? tr("Agent") : it.sender ?? "Zerus")}</b>
          {/* A local row's stamp stands in for the time and the receipt (ActivityView.cpp:1053-1055). */}
          <Show when={it.local} fallback={<span class="act-time">{it.at}</span>}>
            {(l) => <span class="act-stamp" classList={{ failed: !!l().error || l().uncertain }}>{l().stamp}</span>}
          </Show>
          <Show when={it.kind === "user" && it.receipt}><span class="act-receipt"><Icon name="check" size={11} />{it.receipt}</span></Show>
          <span class="grow" />
          <Show when={it.kind === "agent" && !it.excerpt}><button class="icon-btn small act-copy" title={tr("Copy as Markdown")} onClick={() => copyText(it.body ?? "", tr("The reply is on the clipboard"))}><Icon name="copy" size={12} /></button></Show>
        </header>
        {/* A fork's inherited row belongs to its source: never this session's attachments. */}
        <EchoBody item={it} scope={rowScope(it, props.scope)} />
        <Show when={it.kind === "agent" && !it.inherited && it.body}><ReplyMedia text={it.body!} /></Show>
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

/**
 * The search hit Activity shows instead of the session's history: the one
 * chosen for the selected row, its run and conversation, outside a subagent.
 */
export function shownSearchPreview(row: Session | undefined = selected()) {
  const p = searchPreview();
  return p && row && p.key === row.key && p.run === (row.runId ?? "") && p.conversation === (row.conversationId ?? "") && !subagentOpen() ? p : undefined;
}

/** Scrolls to the newest item and follows it again; a shown search hit gives way to the latest activity. */
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
    const key = draftKey({ computer: s.computer, key: s.catalogKey, name: s.name, archive_id: null, session_id: s.sessionId ?? null,
      subagent: { agent_id: child, parent_conversation_id: s.conversationId ?? "" } });
    return v.subagent?.agent_id === child && v.key === key ? v.local : [];
  }
  if (source.readOnly) return [];
  const key = draftKey({ computer: s.computer, key: s.catalogKey, name: s.name, archive_id: s.state === "archived" ? s.archiveId ?? null : null,
    session_id: s.sessionId ?? null });
  // A held "Compact and continue" message shows as its pending entry instead (zerus-pabah).
  return v.key === key ? withoutHeld(v.local, v.held) : [];
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

/**
 * The transcript of `source` (the live follow by default). `session` is the row
 * it shows when that is not the selected session (the Team tab's coordinator).
 */
export function ActivityView(props: { source?: ActivitySource; session?: () => Session | undefined }) {
  const s = () => (props.session ? props.session() : selected());
  const preview = () => shownSearchPreview(s());
  /** The live follow unless the detail passes another source (an archived row's). */
  const source = (): ActivitySource => props.source ?? activity;
  let scroller!: HTMLDivElement;
  const [placed, setPlaced] = createSignal(false);
  const [restoring, setRestoring] = createSignal<ActivityPosition>();
  let restoredPage = false, windowEvents: readonly ActivityEvent[] = [], showingPreview = false;
  const [layoutHeld, setLayoutHeld] = createSignal(false);
  // Measure new/changed loaded rows while the layout gate is idle. A first
  // estimate correction above the viewport must never move an active reader.
  const [measuring, setMeasuring] = createSignal<number[]>([]);
  const [selectionIndexes, setSelectionIndexes] = createSignal<number[]>([]);
  let measuredContent = new Map<string, string>();
  const heights = new ActivityHeights();
  let measurementWidth = 0;
  const [resizing, setResizing] = createSignal(false);
  const overscan = () => activityOverscan(resizing());
  // Items keep their identity while events append, including across the virtual window.
  const [view, setView] = createStore<{ items: TranscriptItem[]; expired: boolean; loaded: boolean; pane: ReturnType<typeof paneStates>; tool: string }>({ items: [], expired: false, loaded: false, pane: paneStates(source(), s()), tool: "" });
  const local = createMemo(() => localRows(source(), s()), undefined, { equals: (a, b) => a === b || (a.length === 0 && b.length === 0) });
  const project = createActivityProjections();
  const displayTarget = () => source().displayKey?.() ?? source().target()?.id;
  // The service's submitted question routes of the live local session, never
  // a subagent's or an archive's (zerus-sa9qn.33). Read at most once per
  // window while its activity changes, and a child slowly as well.
  const routeSession = () => routeSessionOf(s(), { archive: !!props.source, subagent: !!subagentOpen(), target: source().target()?.id });
  // Only the live view reads them; an archive's view never starts or clears the read.
  if (!props.source) {
    createEffect(() => { source().events(); refreshRouteActivity(routeSession(), !!s()?.parent); });
    onCleanup(() => refreshRouteActivity(undefined));
  }
  const built = createMemo(() => {
    const items = project(!!subagentOpen(), displayTarget(), source().events(), source().transcriptSummary?.() ?? source().summary(), local(),
      subagentOpen() ? [] : routeActivityOf(routeSession()));
    // Task 244: a lead session shows team notices and its replies to them as its mode says.
    const row = s();
    return row && !subagentOpen() && isLeadSession(row) ? foldNoticeTurns(items, leadNoticeMode(row)) : items;
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
      syncActivityHeights(heights, measuredContent, content);
      const pending = new Set(measuring().map((index) => view.items[index]?.id));
      setMeasuring(items.flatMap((item, index) => pending.has(item.id) || content.get(item.id) !== measuredContent.get(item.id) ? [index] : []));
      measuredContent = content;
      for (const key of virtualizer.itemSizeCache.keys()) if (!content.has(String(key))) virtualizer.itemSizeCache.delete(key);
      const saved = restoring();
      const disclosures = Object.fromEntries(Object.entries({ ...expanded, ...saved?.expanded }).filter(([key]) => content.has(key)));
      setExpanded(reconcile(disclosures));
      setMore(reconcile(Object.fromEntries(Object.entries({ ...more, ...saved?.more }).filter(([key]) => content.has(key.slice(0, key.lastIndexOf("#")))))));
      setView("items", reconcile(items, { key: "id" }));
      setView({ expired, loaded, pane, tool });
      windowEvents = source().events();
      reading.changed(); end.changed();
    }));
  });
  const [expanded, setExpanded] = createStore<Record<string, boolean>>({});
  /** Show more per tool event (`outputKey`), kept while its row is loaded so a row scrolled back keeps its height. */
  const [more, setMore] = createStore<Record<string, number>>({});
  /** Show more per event of the shown search hit (zerus-tso9j): the same steps, kept only for that hit. */
  const [hitMore, setHitMore] = createSignal<Record<string, number>>({});
  createEffect(on(() => preview()?.hit, () => setHitMore({}), { defer: true }));
  const moreHit = (id: string, index: number) => setHitMore((m) => { const key = outputKey(id, index); return { ...m, [key]: (m[key] ?? TOOL_OUTPUT.first) + TOOL_OUTPUT.step }; });
  /**
   * View process from the live main Activity while Processes is on
   * (ActivityView.cpp:1129-1134): only in the Processes view of the session
   * the tab follows now, checked when offered and again when used (zerus-u81tf).
   */
  const processView = () => { const target = processesTarget(); return target ? processViewKey(target) : undefined; };
  const processLinks = (): ProcessLinks | undefined => source() === activity && !subagentOpen() && flag("processes/enabled") ? {
    linkable: (process) => recordedProcess(processes.view(), process, processView()),
    open: (process) => { if (processes.reveal(process, processView())) setDetailTab("processes"); },
  } : undefined;
  /** The guidance an empty Activity shows (ActivityView.cpp:1145-1150). */
  const empty = createMemo(() => emptyActivity(source().summary(), !!s()?.tracked));
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
    estimateSize: (index) => heights.get(view.items[index]?.id, measurementWidth) ?? 96,
    measureElement: (element) => {
      const rect = element.getBoundingClientRect(), item = view.items[Number(element.getAttribute("data-index"))];
      if (item) heights.set(item.id, rect.width, rect.height);
      return rect.height;
    },
    get getItemKey() { return itemKey(); },
    get rangeExtractor() { return rangeExtractor(); },
    get overscan() { return overscan(); },
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
      const indexes = new Set(pending);
      measureActivityRows([...scroller.querySelectorAll<HTMLElement>(".act-virtual-row")].filter(row => indexes.has(Number(row.dataset.index))), (index, height, width) => {
        const item = view.items[index];
        if (item) heights.set(item.id, width, height);
        virtualizer.resizeItem(index, height);
      });
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
    adjust: (delta) => {
      activityReadingRows(scroller).adjust(delta);
      // The virtualizer learns the position only from the next scroll event,
      // one frame later. A window recomputed before then (premeasured rows
      // dropping after an older page lands) used the anchor's previous,
      // estimated write and painted one blank frame (zerus-oavj).
      virtualizer.scrollOffset = scroller.scrollTop;
    },
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
    setRestoring(undefined);
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
  // Enter on a session row: this view jumps, once per request, even if it mounted after it.
  createEffect(() => { if (takeLatestRequest()) queueMicrotask(jumpToLatest); });
  onMount(() => {
    liveScroller = scroller;
    liveLatest = latest;
    liveReply = showsReply;
    const pointerEnded = () => motion.release();
    window.addEventListener("pointerup", pointerEnded); window.addEventListener("pointercancel", pointerEnded); window.addEventListener("blur", pointerEnded);
    onCleanup(() => { window.removeEventListener("pointerup", pointerEnded); window.removeEventListener("pointercancel", pointerEnded); window.removeEventListener("blur", pointerEnded); });
    activityTexts.set(scroller, () => activityText(view.items, expanded, more));
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
    const column = scroller.querySelector<HTMLElement>(".act-col");
    measurementWidth = column?.getBoundingClientRect().width ?? scroller.clientWidth;
    const measureWindow = () => {
      // Off-screen rows keep their last height as an estimate until mounted.
      // Restoring a width must not clear the entire virtual layout or mount it.
      const pending = activityMeasureWindow(virtualizer.range, view.items.length, overscan());
      batch(() => {
        for (const index of pending) {
          const size = heights.get(view.items[index].id, measurementWidth);
          if (size !== undefined) virtualizer.resizeItem(index, size);
        }
        setMeasuring(pending);
      });
    };
    let viewportWidth = scroller.clientWidth;
    // A capped column can stay fixed while the surrounding viewport resizes.
    const widths = createActivityResize(setResizing, () => { end.settle(); settleReading(); });
    const resize = new ResizeObserver((entries) => {
      const viewport = entries.find(entry => entry.target === scroller)?.contentRect;
      const viewportChanged = viewport && Math.abs(viewport.width - viewportWidth) > .5;
      if (viewport) viewportWidth = viewport.width;
      // These borderless boxes already have current geometry. A synchronous
      // DOM read can flush another whole-page layout after virtual row writes.
      const width = entries.find(entry => entry.target === (column ?? scroller))?.contentRect.width ?? measurementWidth;
      if (viewportChanged && placed()) widths.resized();
      if (Math.abs(width - measurementWidth) > .5) {
        measurementWidth = width;
        if (placed()) { widths.resized(); measureWindow(); }
      }
      end.settle(); settleReading();
    });
    resize.observe(scroller);
    const mutations = new MutationObserver(changed);
    if (column) {
      resize.observe(column);
      mutations.observe(column, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["class", "style", "src", "width", "height", "hidden", "open"] });
    }
    scroller.addEventListener("load", changed, true);
    scroller.addEventListener("contentvisibilityautostatechange", changed, true);
    const fontChanged = () => {
      heights.clear();
      if (placed()) measureWindow();
      changed();
    };
    document.fonts?.addEventListener("loadingdone", fontChanged);
    onCleanup(() => { widths.dispose(); document.removeEventListener("selectionchange", selectionChanged); resize.disconnect(); mutations.disconnect(); scroller.removeEventListener("load", changed, true); scroller.removeEventListener("contentvisibilityautostatechange", changed, true); document.fonts?.removeEventListener("loadingdone", fontChanged); });
  });
  onCleanup(() => {
    savePosition();
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
  const savePosition = () => {
    if (!layoutTarget || !placed() || showingPreview) return;
    const anchors = activityReadingRows(scroller).visible();
    activityPositions.set(layoutTarget, { following: end.active(), anchors,
      after: activityPositionCursor(windowEvents, anchors), seq: activityAnchorSeq(windowEvents, anchors), expanded, more });
  };
  const resetTarget = () => {
    const target = displayTarget();
    if (layoutTarget === target) return;
    savePosition();
    layoutTarget = target;
    const saved = activityPositions.get(target);
    setRestoring(saved && !saved.following ? saved : undefined);
    restoredPage = false; windowEvents = [];
    // An explicit session switch ends its old selection. Do this before the
    // first new projection, so a held range cannot defer a cached first page.
    if (selectionInside()) document.getSelection()?.removeAllRanges();
    sendIds = undefined; setPlaced(false);
    motion.reset(); setLayoutHeld(false); latestAfterScroll = false;
    activityAll.delete(scroller);
    setExpanded(reconcile({}));
    setMore(reconcile({}));
    measuredContent.clear(); setMeasuring([]); setSelectionIndexes([]);
    heights.clear();
    virtualizer.measure();
    setFollowing(!restoring()); setUnseen(0); reading.reset(); newest = undefined;
    // Selection changes must place even when the next page has the same row count.
    if (restoring()) end.release(); else end.pin();
  };
  createEffect(on(displayTarget, resetTarget));
  createEffect(() => {
    const saved = restoring();
    if (!saved || !view.loaded || measuring().length || !markdownReady()) return;
    const hasAnchor = saved.anchors.some(anchor => view.items.some(item => `item-${item.id}` === anchor.key));
    const shown = source();
    // A session visit starts with the newest tail. Re-read the saved
    // reading window once, using its cursor; never keep transcripts per session
    // or chase missing/expired rows through repeated older reads.
    const windowStart = activityPositionCursor(windowEvents, []);
    if (!hasAnchor && !restoredPage && saved.after !== undefined && windowStart !== undefined
      && saved.after < windowStart && readsOlder(shown)) {
      restoredPage = true;
      shown.loadOlder(saved.after);
      return;
    }
    if (shown.loadingOlder()) return;
    const target = displayTarget();
    const frame = requestAnimationFrame(() => {
      if (target !== displayTarget() || restoring() !== saved) return;
      let top: number | undefined, anchors = saved.anchors;
      for (const anchor of saved.anchors) {
        const offset = measuredOffset(anchor.key);
        if (offset !== undefined) { top = scroller.scrollTop + offset - anchor.offset; break; }
      }
      if (top === undefined) {
        // No saved row came back. The transcript around it changed, so never
        // restore its old pixel offset: put the nearest surviving message at
        // or before the saved top row at the top, still released. With none
        // left (a cleared or fully expired transcript), follow Latest.
        const key = activityNearestRow(windowEvents, view.items.map(item => `item-${item.id}`), saved.seq);
        const offset = key === undefined ? undefined : measuredOffset(key);
        if (key === undefined || offset === undefined) {
          batch(() => { setRestoring(undefined); setFollowing(true); setUnseen(0); });
          reading.reset(); end.pin();
          return;
        }
        top = scroller.scrollTop + offset; anchors = [{ key, offset: 0 }];
      }
      scroller.scrollTo({ top, behavior: "instant" });
      virtualizer.scrollOffset = scroller.scrollTop;
      // The offset alone recomputes nothing. A scroll event reports a changed
      // offset only after this paint, and an unchanged one (a position clamped
      // at the end) never: recompute the window now, so the restored position
      // is never revealed with the previous window's rows (zerus-2n2vk).
      virtualizer.measure();
      reading.restore(anchors);
      batch(() => { setRestoring(undefined); setPlaced(true); });
    });
    onCleanup(() => cancelAnimationFrame(frame));
  });
  createEffect(on(local, (rows) => {
    const sends = newLocalSends(sendIds, rows.map((row) => row.id));
    sendIds = sends.baseline;
    if (sends.added.length) jumpToLatest();
  }));
  createEffect(on(preview, (match) => {
    showingPreview = !!match;
    if (match) {
      setRestoring(undefined);
      setPlaced(true);
      motion.reset(); setLayoutHeld(false); end.release(); reading.reset(); setFollowing(false);
      requestAnimationFrame(() => { if (!motion.active() && !selectionInside()) scroller.querySelector(".search-preview mark")?.scrollIntoView({ block: "center" }); });
    } else if (!restoring()) { setFollowing(true); end.pin(); }
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
  const states = () => view.pane;
  // Qt parity (I5d): the working line (WorkingLine) while the selected live
  // session works: what it does now and how long the turn has run.
  const working = createMemo(() => {
    const row = s();
    if (!row || source().readOnly || subagentOpen()) return undefined;
    // A pane's own session (the team lead in Team Designer and New Team, owner
    // feedback 2026-11-14) reads its working state from the source it follows.
    const details = props.session ? source().summary() : followedFor(row.key).value;
    const line = workingLine(details, { online: !computerOffline(row.computer), state: row.state, processState: row.processState }, !props.session && !!composer.view()?.inspect_error);
    const started = Number(details?.turn_started);
    return line === undefined ? undefined : { line, started: Number.isFinite(started) && started > 0 ? started : 0 };
  });
  // The turn's clock ticks each second only while the line shows a start.
  const [now, setNow] = createSignal(Date.now() / 1000);
  createEffect(on(() => !!working()?.started, (ticking) => {
    if (!ticking) return;
    setNow(Date.now() / 1000);
    const timer = setInterval(() => setNow(Date.now() / 1000), 1000);
    onCleanup(() => clearInterval(timer));
  }));
  const fileSession = () => {
    const id = source().target()?.id;
    return orderedRows().find((row) => row.key === id);
  };
  // This Activity's Remove review: retired with it (zerus-5i2e5).
  const inboxScope = createInboxRemoveScope(inboxPorts);
  return (
    <InboxScope.Provider value={inboxScope}>
    <ActivitySession.Provider value={fileSession}>
    <ActivityLayoutHeld.Provider value={layoutHeld}>
    <div class="activity" style={{ position: "relative" }}>
      <Show when={!placed()}>
        <div class="act-col" style={{ position: "absolute", inset: "0", "min-height": "0", "pointer-events": "none", "z-index": "1" }} role="status">
          <div class="act-label eyebrow">{tr("Recorded activity — messages and tool excerpts")}</div>
          {/* Placement waits for a first page, which a shell session and a
              stalled read never have: their states show here, above the
              hidden column and its scroller, and take clicks (zerus-soudn). */}
          <Show when={!states().archived && !view.loaded && !view.items.length} fallback={<div class="act-older hint">{tr("Reading activity…")}</div>}>
            <div style={{ "pointer-events": "auto" }}><ActivityPending source={source()} session={s()} /></div>
          </Show>
        </div>
      </Show>
      <LocalDeleteDialog />
      <RemoveQueuedDialog scope={inboxScope} />
      <LazyBoundary floating><SessionFileDialog /></LazyBoundary>
      <MediaViewerDialog />
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
            {/* The message as Activity shows it, Markdown and links with the query marked (ActivityView.cpp:664, 1592-1605). */}
            <div class="search-preview-body">
              <HighlightQuery.Provider value={() => p().query}>
                <For each={searchHitItems(p().hit)}>{(item) => <Item item={item} scope={scope()} sourceId={source().target()?.id} postersEnabled={false} expanded={item.kind === "tool" || item.kind === "tools"}
                  shown={(index) => hitMore()[outputKey(item.id, index)]} onMore={(index) => moreHit(item.id, index)} />}</For>
              </HighlightQuery.Provider>
            </div>
          </section>}</Show>
          <Show when={!preview()}>
          <div class="act-history-header">
          {/* The two labels swap with "inherit", never "visible": a child set
              visible shows through the column that stays hidden until it is
              placed, so the loading heading was drawn twice (zerus-ig5q). */}
          <div class="act-label eyebrow" style={{ position: "relative", flex: "1", "min-width": "0" }}>
            <span style={{ visibility: source().loadingOlder() ? "hidden" : "inherit" }}>{tr("Recorded activity — messages and tool excerpts")}</span>
            <span class="act-older hint" style={{ position: "absolute", inset: "0", padding: "0", display: "flex", "align-items": "center", "justify-content": "center", "text-transform": "none", "letter-spacing": "normal", "font-weight": "normal", visibility: source().loadingOlder() ? "inherit" : "hidden" }} aria-hidden={!source().loadingOlder()}>{tr("Reading earlier activity…")}</span>
          </div>
          <Show when={!source().olderDone() && source().events().length > 0}>
            <button type="button" class="btn small ghost act-read-older" disabled={source().loadingOlder()} onClick={() => source().loadOlder()}>{tr("Read earlier activity")}</button>
          </Show>
          {/* Task 244: how this lead session shows the team's notices and its replies to them. */}
          <Show when={s() && !subagentOpen() && isLeadSession(s())}>
            <label class="act-notice-mode" title={tr("How the team's notices to the lead and its replies to them show here. The lead receives them all.")}>
              <span class="muted">{tr("Team notices")}</span>
              <select class="field small" data-notice-mode value={leadNoticeMode(s()!)} onChange={(event) => setLeadNoticeMode(s()!, event.currentTarget.value as NoticeMode)}>
                <For each={NOTICE_MODES}>{(mode) => <option value={mode}>{tr(NOTICE_LABEL[mode])}</option>}</For>
              </select>
            </label>
          </Show>
          </div>
          <Show when={view.expired}>
            <div class="act-older hint">{tr("Older events have expired from the journal.")}</div>
          </Show>
          <Show when={!states().archived} fallback={<div class="notice"><Icon name="archive" size={14} /><div>{tr("This conversation is archived.")}</div></div>}>
            <Show when={view.loaded || view.items.length} fallback={<ActivityPending source={source()} session={s()} />}>
              <Show when={view.items.length} fallback={<div class="act-empty"><b>{empty().title}</b><p class="muted">{empty().text}</p></div>}>
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
                        ref={(node) => {
                          const index = indexOf().get(id) ?? 0;
                          node.dataset.index = String(index);
                          const size = heights.get(id, measurementWidth);
                          if (size !== undefined) virtualizer.resizeItem(index, size);
                          virtualizer.measureElement(node);
                        }}
                        style={{ "margin-top": `${flow().margins.get(id) ?? 0}px` }}>
                        <Show when={item()}>{(it) => <Item item={it()} scope={scope()} sourceId={source().target()?.id} postersEnabled={placed() && !pageHidden()} expanded={!!expanded[id]} onExpanded={(open) => { heights.invalidate(id); setExpanded(id, open); }}
                          shown={(index) => more[outputKey(id, index)]}
                          onMore={(index) => { heights.invalidate(id); setMore(outputKey(id, index), (more[outputKey(id, index)] ?? TOOL_OUTPUT.first) + TOOL_OUTPUT.step); }}
                          process={processLinks()} />}</Show>
                      </div>
                    );
                  }}</For>
                  </div>
                  <div class="act-virtual-spacer" style={{ height: `${Math.max(0, virtualizer.getTotalSize() - (virtualItems().at(-1)?.end ?? 0))}px` }} />
                </div>
              </Show>
            </Show>
            <Show when={working()}>{(shown) => (
              <div class="act-live" role="status" title={shown().line} aria-label={tr("Working: %1", shown().line)}>
                <span class="typing"><i /><i /><i /></span><b class="ell">{shown().line}</b>
                <Show when={shown().started}><span class="act-live-time tabular">{formatElapsed(now() - shown().started)}</span></Show>
              </div>
            )}</Show>
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
    </InboxScope.Provider>
  );
}
