import { Index, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { PickerView } from "./opencode-models";
import { composing } from "../lib/keys";
import { SEARCH_LIMIT, SEARCH_PAUSE_MS, enterRow, pickerValues, popoverBox, starText, stepRow } from "./model-picker";
import { tr } from "../../i18n";

// OpenCode's model popover (NewSessionDialog::openModelPicker 1266-1274 with
// OpenCodeModelPicker.cpp), from the prototype's layout (design/webui/src/
// dialogs/setup/ModelPicker.tsx) where Qt does not differ. The list's rules
// are opencode-models.ts's `ModelPicker`, held by the store; this renders its
// view and sends its clicks. Opening it reads the folder's catalog once (the
// store's openModelPicker); only a star asks for a change, through the store.
// Like Qt's popup it closes on Escape or a click outside it.

/** What the popover sends (the store's picker actions). */
export interface ModelPickerActions {
  click(value: string, star: boolean): void;
  select(value: string): void;
  toggleAll(): void;
  search(text: string): void;
  useNative(): void;
  close(): void;
}

/** The grouped list with search, stars, All models and its notice (OpenCodeModelPicker). */
export function ModelList(props: { view: () => PickerView; act: ModelPickerActions; disabled?: boolean }) {
  let list!: HTMLDivElement;
  let search!: HTMLInputElement;
  const view = props.view;
  const [text, setText] = createSignal(view().query);
  const [focused, setFocused] = createSignal<string | undefined>(view().selected);
  const index = createMemo(() => new Map(pickerValues(view()).map((value, i) => [value, i])));
  let timer: ReturnType<typeof setTimeout> | undefined;
  const flush = () => {
    if (timer === undefined) return;
    clearTimeout(timer);
    timer = undefined;
    props.act.search(text());
  };
  onMount(() => queueMicrotask(() => search.focus()));
  // The field keeps a search typed just before closing, as Qt's does.
  onCleanup(flush);
  createEffect(() => {
    const at = focused();
    if (at === undefined) return;
    const i = index().get(at);
    if (i !== undefined) list.querySelector(`#ocm-o-${i}`)?.scrollIntoView({ block: "nearest" });
  });
  const onSearch = (value: string) => {
    setText(value);
    if (timer !== undefined) clearTimeout(timer);
    timer = setTimeout(() => { timer = undefined; props.act.search(value); }, SEARCH_PAUSE_MS);
  };
  const listKey = (e: KeyboardEvent) => {
    if (props.disabled || e.target !== list) return;
    const by = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : e.key === "PageDown" ? 10 : e.key === "PageUp" ? -10
      : e.key === "Home" ? -Infinity : e.key === "End" ? Infinity : 0;
    if (by !== 0) {
      e.preventDefault();
      const values = pickerValues(view());
      if (!values.length) return;
      if (by === -Infinity) setFocused(values[0]);
      else if (by === Infinity) setFocused(values[values.length - 1]);
      else setFocused(stepRow(view(), focused() ?? view().selected, by));
      return;
    }
    if (e.key === "Enter" && !composing(e)) {
      // Enter chooses the focused model row, as a click would.
      e.preventDefault();
      const value = enterRow(view(), focused());
      if (value !== undefined) props.act.select(value);
    }
  };
  const activeId = () => {
    const at = focused();
    const i = at === undefined ? undefined : index().get(at);
    return i === undefined ? undefined : `ocm-o-${i}`;
  };
  return (
    <div class="ocm">
      <input ref={search} disabled={props.disabled} class="field" type="search" placeholder={tr("Search models")} aria-label={tr("Search models")} maxLength={SEARCH_LIMIT} value={text()}
        onInput={(e) => onSearch(e.currentTarget.value)}
        onKeyDown={(e) => {
          // Escape closes the popup, as Qt's: WebKit's search field would only clear itself.
          if (e.key === "Escape") { e.preventDefault(); props.act.close(); }
        }} />
      <div class="ocm-list" ref={list} role="listbox" aria-label={tr("Models")} tabindex="0" aria-activedescendant={activeId()} onKeyDown={listKey}>
        <Index each={view().groups}>{(group) => (
          <div role="group" aria-label={group().label}>
            <div class="ocm-group" aria-hidden="true">{group().label}</div>
            <Index each={group().models}>{(row) => (
              <div class="ocm-row" id={`ocm-o-${index().get(row().value)}`} role="option" aria-selected={row().value === view().selected} aria-disabled={props.disabled || row().unavailable}
                classList={{ on: row().value === view().selected, active: row().value === focused(), gone: row().unavailable }} title={row().tooltip}
                onClick={() => { setFocused(row().value); if (!props.disabled) props.act.click(row().value, false); }}>
                <span class="ocm-name ell">{row().label}</span>
                <Show when={row().marks}><span class="ocm-marks">{row().marks}</span></Show>
                <span class="ocm-star" role="button" aria-label={row().starTooltip} title={row().starTooltip}
                  tabindex={view().favoritesAvailable && !props.disabled ? 0 : -1}
                  aria-disabled={props.disabled || !view().favoritesAvailable}
                  classList={{ on: row().favorite, off: props.disabled || !view().favoritesAvailable }}
                  onKeyDown={(e) => { if ((e.key === "Enter" || e.key === " ") && !props.disabled) { e.preventDefault(); e.stopPropagation(); props.act.click(row().value, true); } }}
                  onClick={(e) => { e.stopPropagation(); if (!props.disabled) props.act.click(row().value, true); }}>{starText(row())}</span>
              </div>
            )}</Index>
          </div>
        )}</Index>
      </div>
      <div class="ocm-foot">
        <button type="button" class="btn small" aria-pressed={view().all.checked} classList={{ on: view().all.checked }} disabled={props.disabled || !view().all.enabled}
          title={view().all.tooltip} onClick={() => props.act.toggleAll()}>{tr("All models")}</button>
        <Show when={view().notice}><span class="ocm-notice" role="status">{view().notice}</span></Show>
      </div>
    </div>
  );
}

/**
 * The popover under the model button: the list, then Use native default
 * model. It is its own modal layer over New session; `anchor` is the model
 * button it opens under and gives the focus back to.
 */
export function ModelPopover(props: { view: () => PickerView; act: ModelPickerActions; anchor: () => HTMLElement | null }) {
  let el!: HTMLDialogElement;
  let gone = false;
  const place = () => {
    const button = props.anchor()?.getBoundingClientRect();
    if (!button) return;
    const box = popoverBox(button, { width: window.innerWidth, height: window.innerHeight });
    Object.assign(el.style, { left: `${box.left}px`, top: `${box.top}px`, width: `${box.width}px`, height: `${box.height}px` });
  };
  onMount(() => {
    place();
    el.showModal();
    window.addEventListener("resize", place);
  });
  onCleanup(() => {
    gone = true;
    window.removeEventListener("resize", place);
    if (el.open) el.close();
    queueMicrotask(() => props.anchor()?.focus());
  });
  const close = () => { if (!gone) props.act.close(); };
  // A press outside the popover closes it, as Qt's popup.
  const outside = (e: PointerEvent) => {
    if (e.target !== el) return;
    const box = el.getBoundingClientRect();
    if (e.clientX < box.left || e.clientX > box.right || e.clientY < box.top || e.clientY > box.bottom) close();
  };
  // Its buttons are no default ones: Enter on them does nothing, nor in the search.
  const keys = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement;
    if (e.key === "Enter" && (target.tagName === "BUTTON" || target.tagName === "INPUT")) e.preventDefault();
  };
  return (
    <dialog class="ns-model-pop" ref={el} aria-label={tr("Model")} onPointerDown={outside} onKeyDown={keys}
      onCancel={(e) => { e.preventDefault(); close(); }} onClose={close}>
      <ModelList view={props.view} act={props.act} />
      <button type="button" class="btn small ns-model-native" onClick={() => props.act.useNative()}>{tr("Use native default model")}</button>
    </dialog>
  );
}
