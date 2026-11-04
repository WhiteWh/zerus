import { For, Index, Match, Show, Switch, createEffect, createMemo, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { ProviderGlyph } from "../sessions/SessionCard";
import { agentKind } from "../../store/catalog";
import { menuItemActivates } from "../sessions/actions-ui";
import { menu, visibleMenu, setMenu, type MenuEntry } from "../../store/ui";
import { dismissNotice, notices } from "../../store/notices";
import { tr } from "../../i18n";

/** Context and dropdown menus share one positioned popover. */
export function MenuLayer() {
  let el: HTMLDivElement | undefined;
  createEffect(() => {
    if (!menu()) return;
    const close = (e: Event) => {
      if (e instanceof KeyboardEvent && e.key !== "Escape") return;
      if (e instanceof MouseEvent && el?.contains(e.target as Node)) return;
      setMenu(undefined);
    };
    const t = setTimeout(() => {
      addEventListener("mousedown", close);
      addEventListener("keydown", close);
      addEventListener("blur", close);
    });
    onCleanup(() => {
      clearTimeout(t);
      removeEventListener("mousedown", close);
      removeEventListener("keydown", close);
      removeEventListener("blur", close);
    });
  });
  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const items = [...(el?.querySelectorAll<HTMLElement>(".menu-item:not([aria-disabled=true])") ?? [])];
    const i = items.indexOf(document.activeElement as HTMLElement);
    items[(i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
  };
  return (
    <Show when={visibleMenu()}>
      {(m) => (
        <MenuPopup menu={m} reference={(node) => { el = node; }} onKey={onKey} />
      )}
    </Show>
  );
}

/** Measure the final layout while hidden; never measure an animated transform. */
function MenuPopup(props: { menu: () => NonNullable<ReturnType<typeof visibleMenu>>; reference: (el: HTMLDivElement) => void; onKey: (e: KeyboardEvent) => void }) {
  let el!: HTMLDivElement;
  // Item replies update the existing slots without measuring or moving the
  // open menu. Only a new anchor changes its placement.
  const x = createMemo(() => props.menu().x), y = createMemo(() => props.menu().y);
  createEffect(() => {
    el.style.left = `${Math.max(8, Math.min(x(), innerWidth - el.offsetWidth - 8))}px`;
    el.style.top = `${Math.max(8, Math.min(y(), innerHeight - el.offsetHeight - 8))}px`;
    el.style.visibility = "visible";
  });
  onMount(() => el.querySelector<HTMLElement>(".menu-item:not([aria-disabled=true])")?.focus({ preventScroll: true }));
  return (
    <div class="menu" role="menu" data-shortcut-context="none" ref={(node) => { el = node; props.reference(node); }} style={{ visibility: "hidden" }} onKeyDown={props.onKey}>
      <Index each={props.menu().items}>{(item) => <MenuItem item={item} />}</Index>
    </div>
  );
}

/** Position-based slots retain buttons and focus across async scope counts. */
function MenuItem(props: { item: () => MenuEntry }) {
  const action = () => { const item = props.item(); return "label" in item ? item : undefined; };
  const heading = () => { const item = props.item(); return "heading" in item ? item.heading : undefined; };
  return <Switch>
    <Match when={"separator" in props.item()}><div class="menu-sep" role="separator" /></Match>
    <Match when={heading()}><div class="menu-label">{heading()}</div></Match>
    <Match when={action()}>{(item) => (
      <button class="menu-item" classList={{ danger: item().danger }} role={item().checked !== undefined ? "menuitemcheckbox" : "menuitem"}
        aria-checked={item().checked} aria-disabled={item().disabled} title={item().hint}
        onClick={() => { const entry = item(); if (!menuItemActivates(entry)) return; setMenu(undefined); entry.action?.(); }}>
        <Show when={item().checked !== undefined} fallback={<Show when={item().icon} fallback={<span style={{ width: "14px" }} />}><Icon name={item().icon!} size={14} /></Show>}>
          <Icon name={item().checked && item().checked !== "mixed" ? "check" : "minus"} size={14} style={{ opacity: item().checked ? 1 : 0 }} />
        </Show>
        <Show when={item().agent}><span class="menu-agent" style={{ color: `var(--z-agent-${agentKind(item().agent)})` }}><ProviderGlyph agent={agentKind(item().agent)} size={13} /></span></Show>
        <span>{item().label}</span>
        <Show when={item().kbd}><span class="kbd">{item().kbd}</span></Show>
      </button>
    )}</Match>
  </Switch>;
}

/**
 * The window's one notice bus, in the design's bottom-right corner: copies,
 * action results, updates and refusals. Notices let clicks through to the
 * controls under them; only a sticky notice's Dismiss takes a click.
 */
export function Toasts() {
  return (
    <div class="toasts" aria-live="polite">
      <For each={notices()}>
        {(n) => (
          <div class={`toast ${n.tone}`} data-notice-id={n.id} classList={{ sticky: n.sticky }}>
            <Show when={n.sticky && n.dismissible !== false}><button class="icon-btn dismiss" aria-label={tr("Dismiss")} onClick={() => dismissNotice(n.id)}><Icon name="x" size={14} /></button></Show>
            <Show when={n.title}><b>{n.title}</b></Show>
            <span class="note" classList={{ muted: !!n.title }}>{n.text}</span>
            <Show when={n.note?.()}><div class="note muted">{n.note?.()}</div></Show>
            <Show when={n.actions?.length}><div class="toast-actions"><For each={n.actions}>{(action) =>
              <button type="button" class={`btn small ${action.style}`} disabled={action.disabled?.()} onClick={() => { if (!action.disabled?.()) void action.run(); }}>
                <Show when={action.icon}><Icon name={action.icon!} size={13} /></Show>{typeof action.label === "function" ? action.label() : action.label}
              </button>
            }</For></div></Show>
          </div>
        )}
      </For>
    </div>
  );
}
