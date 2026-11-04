import { For, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import { Icon } from "../lib/icons";
import { available, section, setSection, type SectionId } from "./settings/sections";
import { tr } from "../../i18n";

// Settings (tray/src/SettingsPage.h): a fixed section list and one scrollable
// page at a time, at most 850 px wide, as in the approved mockup
// (design/webui/src/screens/Settings.tsx). Up and Down move through the list.

export function SettingsScreen() {
  let scroller!: HTMLDivElement;
  const open = (id: SectionId) => {
    setSection(id);
    available().find((s) => s.id === id)?.refresh?.();
    scroller.scrollTop = 0;
  };
  const current = () => available().find((s) => s.id === section()) ?? available()[0];
  const onNavKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const list = available();
    const i = list.findIndex((s) => s.id === current().id);
    const next = list[(i + (e.key === "ArrowDown" ? 1 : list.length - 1)) % list.length];
    open(next.id);
    document.getElementById(`set-nav-${next.id}`)?.focus();
  };
  return (
    <div class="set-page">
      <div class="set-shell">
        <nav class="set-nav" aria-label={tr("Settings sections")} onKeyDown={onNavKey}>
          <h1>{tr("Settings")}</h1>
          <div role="tablist" aria-orientation="vertical" class="set-nav-list">
            <For each={available()}>
              {(s) => (
                <button id={`set-nav-${s.id}`} role="tab" class="set-nav-item" classList={{ on: current().id === s.id }} aria-selected={current().id === s.id}
                  tabindex={current().id === s.id ? 0 : -1} aria-controls="set-content" onClick={() => open(s.id)}>
                  <Icon name={s.icon} size={15} stroke={1.8} />
                  <span class="ell">{tr(s.label)}</span>
                </button>
              )}
            </For>
          </div>
        </nav>
        <div class="set-scroll" ref={scroller} id="set-content" role="tabpanel" aria-labelledby={`set-nav-${current().id}`}>
          <div class="set-content">
            <Show when={current().component}>{(component) => <Dynamic component={component()} />}</Show>
          </div>
        </div>
      </div>
    </div>
  );
}
