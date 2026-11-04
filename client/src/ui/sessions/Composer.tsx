import { Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { createVoice, voicePort } from "../../store/voice";
import { VoiceToolbar, VoicePreview } from "./VoiceControls";
import { pageHidden } from "../../store/visibility";
import { flag } from "../../store/settings";
import { subagentOpen } from "../../store/ui";
import { Icon } from "../lib/icons";
import { composer } from "../../store/composer";
import { suggestions, suggestedInput } from "../../store/suggestions";
import { contentScale, selected } from "../../store/ui";
import { computerFresh, computerOffline } from "../../store/catalog";
import { detailsFor, followedFor } from "../../store/actions";
import { onCommand } from "../../store/shortcuts";
import { actionEntries } from "./ActionDialogs";
import { snapshotOf } from "./actions-ui";
import { markReadChip, runMarkRead } from "./mark-entries";
import { ModelSettingsButton } from "./ModelSettings";
import { AttachmentsChip } from "./Attachments";
import { RecoveryChip } from "./RecoveryChip";
import { SessionUsageChip, UsageLimitChip } from "./UsageChips";
import { moveFocusOff } from "./composer-ui";
import { focusTranscript, following, jumpToLatest, unseen } from "./Activity";
import type { ChipTone } from "../../types";
import { tr } from "../../i18n";
import { trText } from "../../text";

// The message composer (Qt MessageComposer, 300-366), after the mockup's
// structure: an edge grip, the 34 px toolbar, the editor (76-720 px) and the
// action row. The toolbar keeps Qt's slots (ComposerToolbar.h, Slot): the
// leading zone holds Attachments, the voice slot (Task 13), Mark as read,
// Latest, the compaction indicator and Cancel send; the grip fills the space
// between; the trailing zone holds Recovery, Usage, Cache and Context. The action
// row holds Attach, Reference file, the status line, Refresh, Allow retry,
// the model button, Stop and Send (364). The store (store/composer.ts) owns
// the text and the locks; Rust owns the draft, the attachments and the send.

// Editor height at 100% content scale, 76-720 px, remembered per viewer.
const EDITOR = { initial: 76, min: 76, max: 720 };
const HEIGHT_KEY = "zerus-client-editor-height";
const savedHeight = (() => { try { return Number(localStorage.getItem(HEIGHT_KEY)) || 0; } catch { return 0; } })();
const [editorHeight, setEditorHeightRaw] = createSignal(savedHeight >= EDITOR.min ? Math.min(EDITOR.max, savedHeight) : EDITOR.initial);
const setEditorHeight = (h: number) => {
  const v = Math.round(Math.max(EDITOR.min, Math.min(EDITOR.max, h)));
  setEditorHeightRaw(v);
  try { localStorage.setItem(HEIGHT_KEY, String(v)); } catch { /* storage may be unavailable */ }
};
/** Qt's double-click reset removes the stored height (MessageComposer.cpp:288-290). */
const resetEditorHeight = () => {
  setEditorHeightRaw(EDITOR.initial);
  try { localStorage.removeItem(HEIGHT_KEY); } catch { /* storage may be unavailable */ }
};

/** Below this width the toolbar shortens its chips (Qt's ComposerToolbar fit: short labels first). */
const COMPACT_WIDTH = 600;

const USUAL_STOP = () => tr("Interrupt the current turn, like Escape in Terminal. The session stays open; queued messages follow the agent's native behavior.");
const USUAL_CACHE_CLEAR = () => tr("Clear this agent’s conversation, like /clear in Terminal. Your unsent message stays here.");
const CANCEL_SEND = () => tr("Cancel the pending send and unlock the message for editing. Compaction or startup continues; use Stop to interrupt the agent.");
const ATTACH = () => tr("Attach files, paste an image, or drop files onto this session");

const toneClass = (tone: ChipTone) => (tone === "danger" ? "danger" : tone === "warning" ? "warn" : tone === "success" ? "ok" : "");

/**
 * ComposerResizeHandle: the free space between the leading and trailing chips
 * is the drag area with the grip centred in it; the "edge" variant is an
 * invisible strip along the top border. Drag up to grow the field,
 * double-click or Home to reset, Up and Down resize by 20 px. The field never
 * grows past the room above it: Activity keeps at least 120 px.
 */
function ResizeHandle(props: { edge?: boolean; hidden?: boolean }) {
  const [dragging, setDragging] = createSignal(false);
  const limit = (el: HTMLElement) => {
    const pane = el.closest(".activity-wrap")?.getBoundingClientRect();
    const editor = el.closest(".composer")?.querySelector(".cmp-editor")?.getBoundingClientRect();
    if (!pane || !editor) return EDITOR.max;
    const activity = pane.height - (el.closest(".cmp-col")?.getBoundingClientRect().height ?? 0);
    return Math.max(EDITOR.min, Math.min(EDITOR.max, (editor.height + Math.max(0, activity - 120)) / contentScale()));
  };
  const down = (e: PointerEvent) => {
    if (e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const startY = e.clientY, start = editorHeight(), max = limit(el);
    setDragging(true);
    const move = (m: PointerEvent) => setEditorHeight(Math.min(max, start + (startY - m.clientY) / contentScale()));
    const up = () => { setDragging(false); el.removeEventListener("pointermove", move); el.removeEventListener("pointerup", up); el.removeEventListener("pointercancel", up); };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  };
  const key = (e: KeyboardEvent) => {
    const el = e.currentTarget as HTMLElement;
    if (e.key === "Home") resetEditorHeight();
    else if (e.key === "ArrowUp") setEditorHeight(Math.min(limit(el), editorHeight() + 20));
    else if (e.key === "ArrowDown") setEditorHeight(editorHeight() - 20);
    else return;
    e.preventDefault();
  };
  return (
    <div class="cmp-resize" classList={{ dragging: dragging(), edge: props.edge, gap: !props.edge }} hidden={props.hidden} tabindex={props.edge ? -1 : 0} role="separator" aria-orientation="horizontal"
      aria-label={tr("Message field height")} aria-valuemin={EDITOR.min} aria-valuemax={EDITOR.max} aria-valuenow={editorHeight()}
      onPointerDown={down} onDblClick={resetEditorHeight} onKeyDown={key}>
      <Show when={!props.edge}><i class="cmp-grip" /></Show>
    </div>
  );
}

/** The cache chip and its popover (SessionsWindow.cpp 987-1002, 3789-3795; CacheStatus::Chip). */
function CacheChip(props: { compact: boolean }) {
  const v = () => composer.view()!;
  const [open, setOpen] = createSignal(false);
  const [pos, setPos] = createSignal({ left: 0, bottom: 0 });
  let chip!: HTMLButtonElement;
  let pop: HTMLDivElement | undefined;
  const close = (returnFocus = false) => { setOpen(false); if (returnFocus) chip.focus(); };
  const outside = (e: PointerEvent) => { if (open() && !pop?.contains(e.target as Node) && !chip.contains(e.target as Node)) close(); };
  onMount(() => addEventListener("pointerdown", outside, true));
  onCleanup(() => removeEventListener("pointerdown", outside, true));
  // The chip goes (a session without a cache): so does its popover.
  createEffect(() => { if (!v().cache.visible) setOpen(false); });
  const detail = () => (v().cache_warning ? `${v().cache_warning}\n` : "") + v().cache_details;
  return (
    <Show when={v().cache.visible}>
      <span class="att-anchor">
        <button ref={chip} type="button" class={`tchip ${toneClass(v().cache.tone)}`} classList={{ on: open() }} aria-haspopup="dialog" aria-expanded={open()}
          aria-label={v().cache.label} title={detail()}
          onClick={(e) => {
            const r = chip.getBoundingClientRect();
            setPos({ left: Math.max(8, Math.min(r.left, innerWidth - 368)), bottom: innerHeight - r.top + 6 });
            setOpen(!open());
            if (open()) queueMicrotask(() => (e.detail === 0 ? pop?.querySelector<HTMLElement>("button:not(:disabled)") ?? pop : pop)?.focus({ preventScroll: true }));
          }}>
          <Icon name={v().cache.tone === "success" ? "timer" : "context-warning"} size={13} />
          {props.compact ? v().cache.short_label : v().cache.label}
        </button>
        <Show when={open()}>
          <div class="att-pop cache-pop" ref={pop} tabindex="-1" role="dialog" aria-label={tr("Prompt cache")} data-shortcut-context="none"
            style={{ left: `${pos().left}px`, bottom: `${pos().bottom}px` }}
            onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(true); } }}>
            <Show when={v().cache_warning}><p class="cache-warning">{v().cache_warning}</p></Show>
            <p class="cache-detail">{v().cache_details}</p>
            <div class="cache-actions">
              {/* Shown with a warning only (3793); enabled by the cache rule, not Clear session's (3794). */}
              <Show when={v().cache_warning}>
                <button type="button" class="btn small" disabled={!v().clear.cache_enabled} title={v().clear.cache_reason || USUAL_CACHE_CLEAR()}
                  onClick={() => { close(); composer.openClear(); }}>{tr("Clear context")}</button>
              </Show>
            </div>
          </div>
        </Show>
      </span>
    </Show>
  );
}

export function Composer() {
  const s = () => selected()!;
  const v = () => composer.view();
  const voice = createVoice(voicePort);
  onCleanup(() => { void voice.close(); });
  const child = () => !!v()?.subagent;
  const online = () => !computerOffline(s().computer);
  const details = createMemo(() => detailsFor(s(), online(), followedFor(s().key)));
  createEffect(() => {
    const view = v(), row = s(), d = details();
    voice.follow(view ? { handle: view.handle, key: view.key, visible: !pageHidden() && !subagentOpen() && view.input_visible,
      revision: JSON.stringify([view.loaded, view.sending, view.waiting, view.attaching, view.available, view.block_reason, view.inspect_error, row.runId, row.conversationId, d?.run_id, d?.conversation_id,
        flag("speech/video_mode"), flag("speech/preview_before_send")]) } : undefined);
  });
  const act = createMemo(() => actionEntries(s(), details()));
  const mark = createMemo(() => markReadChip(s(), computerFresh(s().computer)));
  // Qt's compaction indicator (ActivityView.cpp 1899-1901) over the details it is given (3974-3975: an
  // offline host, a failed read or a session not running reads as unavailable).
  const compacting = createMemo(() => {
    const state = followedFor(s().key).value ?? {};
    return !child() && s().tracked && online() && !v()?.inspect_error && s().state === "running" && state.phase === "compacting" && state.runtime_state === "live"
      && state.process_state === "running" && state.state !== "archived";
  });
  createEffect(() => {
    const row = s(), state = followedFor(row.key).value ?? {};
    const raw = state.prompt_suggestion as { text?: unknown; at?: unknown } | undefined;
    const phase = typeof state.phase === "string" ? state.phase : row.phase;
    const active = !child() && phase === "idle" && online() && row.state === "running";
    suggestions.set(row.catalogKey, active && typeof raw?.text === "string" ? raw.text : "", typeof raw?.at === "number" ? raw.at : 0);
  });
  const suggestion = () => child() ? undefined : suggestions.get(s().catalogKey, !!v()?.sending || !!v()?.waiting);
  const [compact, setCompact] = createSignal(false);
  let root!: HTMLDivElement;
  let editor!: HTMLTextAreaElement;

  /** The cursor as Qt keeps it: the anchor and the position. */
  const caret = () => editor.selectionDirection === "backward"
    ? { anchor: editor.selectionEnd, position: editor.selectionStart }
    : { anchor: editor.selectionStart, position: editor.selectionEnd };
  const remember = () => { const c = caret(); composer.select(c.anchor, c.position); };
  // A text Rust hands back (a higher epoch, another composer) brings its cursor; other views leave the field alone.
  const handedBack = createMemo(() => `${v()?.handle ?? ""}:${v()?.text_epoch ?? ""}`);
  createEffect(on(handedBack, () => {
    const sel = composer.selection();
    if (editor.value !== composer.text()) editor.value = composer.text();
    const start = Math.min(sel.start, sel.end), end = Math.max(sel.start, sel.end);
    try { editor.setSelectionRange(start, end, sel.start <= sel.end ? "forward" : "backward"); } catch { /* not rendered */ }
  }));

  const stopTurn = () => {
    if (child()) return false;
    const stop = act().stop;
    if (!stop.shown || stop.disabled) return false;
    void composer.interrupted().then(() => stop.action());
    return true;
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === "Tab" && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
      const next = suggestedInput(composer.text(), suggestion() ?? "", composer.locked());
      if (next) {
        e.preventDefault();
        editor.value = next;
        editor.setSelectionRange(next.length, next.length);
        composer.input(next, next.length, next.length);
      }
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void composer.send(); return; }
    // Like Escape in Terminal: stop a working turn, otherwise leave the field (ComposeEdit::keyPressEvent).
    if (e.key === "Escape") { e.preventDefault(); if (!stopTurn()) editor.blur(); }
  };
  const onPaste = (e: ClipboardEvent) => {
    const items = [...(e.clipboardData?.items ?? [])];
    const file = items.find((i) => i.kind === "file" && i.type.startsWith("image/"))?.getAsFile()
      ?? [...(e.clipboardData?.files ?? [])].find((f) => f.type.startsWith("image/"));
    if (!file) return;
    e.preventDefault();
    void composer.pasteImage(file);
  };

  onMount(() => {
    const stopVoice = (event: KeyboardEvent) => {
      if (!voice.shown()?.recording || event.isComposing || document.querySelector("dialog[open]")) return;
      if (event.code !== "Space" && event.key !== "Escape") return;
      event.preventDefault(); event.stopImmediatePropagation();
      void voice.input({ kind: "stop", forget_send: false });
    };
    const blurVoice = () => { if (voice.shown()?.recording) void voice.input({ kind: "stop", forget_send: true }); };
    addEventListener("keydown", stopVoice, true); addEventListener("blur", blurVoice);
    onCleanup(() => { removeEventListener("keydown", stopVoice, true); removeEventListener("blur", blurVoice); });
    // Composer chords go through the window router (store/shortcuts.ts) while the focus is in the field.
    const stops = [onCommand("message.send", () => { void composer.send(); }, editor)];
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(() => setCompact(root.clientWidth < COMPACT_WIDTH * contentScale()));
    observer?.observe(root);
    onCleanup(() => { for (const stop of stops) stop(); observer?.disconnect(); void composer.flush(); });
  });
  createEffect(() => {
    if (!composer.pushEnabled()) return;
    // Remove the action when disabled, so Cut keeps its keys (Qt 841-853).
    const stop = onCommand("message.push", () => { void composer.push(); }, editor);
    onCleanup(stop);
  });

  const status = () => composer.status();
  const statusDetail = () => {
    const text = status().text, session = v()?.session_status ?? "";
    return session && session !== text ? `${text}\n${session}` : text;
  };
  const jumpCaption = () => (unseen() > 0 ? tr("Jump to latest activity (%1 new)", unseen()) : tr("Jump to latest activity"));
  const editorTip = () => v()?.waiting ?? (v()?.session_blocks_input ? v()!.session_status : undefined);
  // A required question card stands in for the editor, its grip and the action row; the toolbar stays (setInputVisible, 1126-1131).
  // They stay mounted and only hide, as Qt's setVisible: the editor's command, observer and flush need it.
  const input = () => v()?.input_visible !== false;
  return (
    <div class="composer" ref={root} data-shortcut-context="composer" classList={{ blocked: !!v()?.session_blocks_input }}>
      <ResizeHandle edge hidden={!input()} />
      <div class="cmp-toolbar">
        <AttachmentsChip compact={compact()} />
        <VoiceToolbar voice={voice} compact={compact()} />
        <Show when={!child() && mark().active}>
          <button type="button" class="tchip accent" classList={{ icon: compact() }} title={mark().detail} aria-label={tr("Mark as read")}
            onClick={(e) => {
              // The chip goes once read: its focus moves on first (composer-ui.ts moveFocusOff).
              if (document.activeElement === e.currentTarget) moveFocusOff(focusTranscript, editor);
              runMarkRead(snapshotOf(s()));
            }}>
            <Icon name="check-check" size={13} /><Show when={!compact()}>{tr("Mark as read")}</Show>
          </button>
        </Show>
        <Show when={!following()}>
          <button type="button" class="tchip icon" title={jumpCaption()} aria-label={jumpCaption()}
            onClick={(e) => { if (document.activeElement === e.currentTarget) moveFocusOff(focusTranscript, editor); jumpToLatest(); }}><Icon name="chevrons-down" size={14} /></button>
        </Show>
        <Show when={!child() && compacting()}>
          <span class="tchip cmp-compaction" title={tr("The agent is compressing conversation context.")} role="status">
            <span class="cmp-progress" aria-label={tr("Compacting context")} />{tr("Compacting context…")}
          </span>
        </Show>
        <Show when={!child() && v()?.cancel_send}>
          <button type="button" class="tchip link" title={CANCEL_SEND()} onClick={() => { void composer.cancelSend(); editor.focus(); }}>{tr("Cancel send")}</button>
        </Show>
        <ResizeHandle hidden={!input()} />
        <Show when={!input()}><span class="grow" /></Show>
        <Show when={!child()}>
          <RecoveryChip session={s()} compact={compact()} />
          <UsageLimitChip session={s()} compact={compact()} />
          <Show when={v()}><CacheChip compact={compact()} /></Show>
        </Show>
        <SessionUsageChip />
      </div>
      <VoicePreview voice={voice} />
      <textarea ref={editor} class="cmp-editor" hidden={!input()} style={{ height: `${editorHeight() * contentScale()}px` }} aria-label={tr("Message to selected agent")}
        placeholder={suggestion() || tr("Message this agent…")} readOnly={composer.locked()} disabled={!v()} value={composer.text()} title={editorTip()}
        onInput={(e) => { const c = caret(); composer.input(e.currentTarget.value, c.anchor, c.position); }}
        onSelect={remember} onKeyUp={remember} onMouseUp={remember}
        onBlur={() => { void composer.flush(); }} onKeyDown={onKey} onPaste={onPaste} />
      <Show when={input() && composer.hint()}><div class="cmp-hint" role="status">{composer.hint()}</div></Show>
      <div class="cmp-actions" hidden={!input()}>
        <span class="cmp-files">
          <button type="button" class="icon-btn cmp-attach" disabled={!v() || composer.locked() || !v()!.attachments_available} aria-label={tr("Attach files to message")}
            title={v()?.attachments_available === false ? v()!.attachments_reason : ATTACH()}
            onClick={() => { void composer.attachFiles(); }}><Icon name="paperclip" size={17} /></button>
          <button type="button" class="icon-btn cmp-attach" disabled aria-label={tr("Reference an existing file")} title={tr("Arrives with file references.")}><Icon name="file-text" size={16} /></button>
        </span>
        <Show when={status().text}>
          <span class="cmp-status ell" classList={{ error: status().error, waiting: !status().error && !!v()?.waiting }} title={statusDetail()} aria-label={status().text}>{status().text}</span>
        </Show>
        <span class="grow" />
        <Show when={v()?.session_status && v()!.session_status_error}>
          <button type="button" class="icon-btn cmp-attach" aria-label={tr("Refresh this session")} title={`${tr("Refresh this session")}\n${v()!.session_status}`}
            disabled={composer.operationLocked()} onClick={() => { void composer.refresh(); }}><Icon name="refresh-cw" size={17} /></button>
        </Show>
        <Show when={v()?.uncertain && !composer.operationLocked()}>
          <button type="button" class="btn ghost" title={tr("Check the terminal before sending again to avoid a duplicate message")}
            onClick={() => { void composer.allowRetry(); editor.focus(); }}><Icon name="refresh-cw" size={13} />{tr("Allow retry")}</button>
        </Show>
        <Show when={!child()}><ModelSettingsButton /></Show>
        <Show when={!child() && act().stop.shown}>
          <button type="button" class="btn" aria-label={tr("Stop current turn")} title={act().stop.hint ?? USUAL_STOP()} disabled={act().stop.disabled}
            onClick={() => { stopTurn(); }}><Icon name="square" size={12} />{tr("Stop")}</button>
        </Show>
        <button type="button" class="btn primary" aria-label={tr("Send")} disabled={!composer.sendEnabled()} title={v()?.waiting ?? undefined}
          onClick={() => { void composer.send(); }}>{v() ? trText(v()!.send_label) : tr("Send")}<Icon name="arrow-up" size={13} /></button>
      </div>
    </div>
  );
}
