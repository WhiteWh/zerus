import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { keys } from "../../store/shortcuts";
import { activateGoTo, closeGoTo, goToItems, goToOrigin } from "../../store/goTo";
import { goToDetails, goToKey, goToKindLabel, rankGoTo, visibleGoToRows, type GoToKeyState } from "./goTo";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

export function GoToDialog() {
  let field: HTMLInputElement | undefined;
  let frame: HTMLDivElement | undefined;
  const [query, setQuery] = createSignal("");
  const [current, setCurrent] = createSignal(0);
  const [start, setStart] = createSignal(0);
  const closeState: GoToKeyState = { typed: [] };
  const ranked = createMemo(() => rankGoTo(goToItems(), query()));
  const visible = createMemo(() => visibleGoToRows(ranked(), current(), 12, start()));
  createEffect(() => {
    const next = visible().start;
    if (next !== start()) setStart(next);
  });
  createEffect(() => {
    if (!goToOrigin()) return;
    setQuery("");
    setCurrent(0);
    setStart(0);
    closeState.typed = [];
    queueMicrotask(() => field?.focus());
    const outside = (event: MouseEvent) => { if (!frame?.contains(event.target as Node)) closeGoTo(); };
    const t = setTimeout(() => addEventListener("mousedown", outside));
    onCleanup(() => {
      clearTimeout(t);
      removeEventListener("mousedown", outside);
    });
  });
  const onKey = (event: KeyboardEvent) => {
    const action = goToKey(event, closeState, keys("go.anywhere"));
    if (action === "none") return;
    event.preventDefault();
    event.stopPropagation();
    if (action === "close") { closeGoTo(); return; }
    if (action === "hold") return;
    const count = ranked().length;
    if (action === "up") setCurrent(Math.max(0, current() - 1));
    else if (action === "down") setCurrent(Math.min(Math.max(0, count - 1), current() + 1));
    else if (action === "pageUp") setCurrent(Math.max(0, current() - 10));
    else if (action === "pageDown") setCurrent(Math.min(Math.max(0, count - 1), current() + 10));
    else if (action === "activate") {
      const item = ranked()[current()];
      if (item) activateGoTo(item);
    }
  };
  return (
    <Show when={goToOrigin()}>
      <div class="goto-scrim" data-shortcut-context="none">
        <div class="goto" ref={frame} role="dialog" aria-label={tr("Go to")}>
          <div class="goto-field">
            <Icon name="search" size={15} />
            <input ref={field} value={query()} placeholder={tr("Go to… (type > for commands)")} onInput={(event) => { setQuery(event.currentTarget.value); setCurrent(0); }} onKeyDown={onKey} />
            <span class="kbd">Esc</span>
          </div>
          <ul class="goto-list" role="listbox" aria-label={tr("Results")}>
            <For each={visible().rows} fallback={<li class="goto-empty">{tr("Nothing matches")}</li>}>{(item, index) => (
              <li role="option" aria-selected={visible().start + index() === current()} classList={{ on: visible().start + index() === current() }}
                onMouseEnter={() => setCurrent(visible().start + index())} onClick={() => activateGoTo(item)}>
                <span class="goto-kind">{goToKindLabel(item.kind)}</span>
                <span class="ell parts goto-text"><b>{item.title}</b><For each={goToDetails(item.detail)}>{(detail) => <span class="dim">{detail}</span>}</For></span>
                <span class="grow" />
                <Show when={item.chord}><span class="kbd">{item.chord}</span></Show>
              </li>
            )}</For>
          </ul>
        </div>
      </div>
    </Show>
  );
}
