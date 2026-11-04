import { For, Show, type JSX } from "solid-js";
import { Icon } from "../../lib/icons";
import { fieldValue } from "./number-field";
import { tr } from "../../../i18n";

// The rows Settings sections are built from, from the approved mockup
// (design/webui/src/screens/settings/ui.tsx). They stand in for the Qt widgets
// each page uses (QCheckBox, QComboBox, QSlider with its reset button)
// without adding behavior of their own.

export function SectionHead(props: { title: string; intro?: JSX.Element }) {
  return (
    <header class="set-head">
      <h2>{tr(props.title)}</h2>
      <Show when={props.intro}><p class="set-intro">{props.intro}</p></Show>
    </header>
  );
}

export function Group(props: { title?: string; hint?: JSX.Element; aside?: JSX.Element; children: JSX.Element; class?:string }) {
  return (
    <section class={props.class?`set-group ${props.class}`:"set-group"}>
      <Show when={props.title || props.aside}>
        <div class="set-group-head">
          <Show when={props.title}><h3>{tr(props.title!)}</h3></Show>
          <span class="grow" />
          {props.aside}
        </div>
      </Show>
      <Show when={typeof props.hint === "string" ? tr(props.hint) : props.hint}><p class="set-group-hint">{typeof props.hint === "string" ? tr(props.hint) : props.hint}</p></Show>
      {props.children}
    </section>
  );
}

/** A row of buttons and short status lines. */
export function Actions(props: { children: JSX.Element }) {
  return <div class="set-actions">{props.children}</div>;
}

/** A read-only table with one selectable row (QTableWidget, rows selected whole). */
export function Table<T>(props: {
  columns: { key: string; label: string; class?: string; render: (row: T) => JSX.Element }[];
  rows: T[];
  rowKey: (row: T) => string;
  selected?: string;
  onSelect?: (key: string) => void;
  label: string;
  empty?: JSX.Element;
  class?: string;
}) {
  return (
    <div class={`set-table-wrap ${props.class ?? ""}`}>
      <table class="set-table" aria-label={typeof props.label === "string" ? tr(props.label) : props.label}>
        <thead>
          <tr><For each={props.columns}>{(c) => <th class={c.class} scope="col">{tr(c.label)}</th>}</For></tr>
        </thead>
        <tbody>
          <For each={props.rows} fallback={<tr class="set-table-empty"><td colspan={props.columns.length}>{props.empty}</td></tr>}>
            {(row) => {
              const key = props.rowKey(row);
              return (
                <tr classList={{ on: props.selected === key }} tabindex={props.onSelect ? 0 : undefined}
                  aria-selected={props.onSelect ? props.selected === key : undefined}
                  onClick={() => props.onSelect?.(key)}
                  onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); props.onSelect?.(key); } }}>
                  <For each={props.columns}>{(c) => <td class={c.class}>{c.render(row)}</td>}</For>
                </tr>
              );
            }}
          </For>
        </tbody>
      </table>
    </div>
  );
}

/** A labelled setting: text on the left, the control on the right, a hint below. */
export function Row(props: { label: JSX.Element; hint?: JSX.Element; children?: JSX.Element; for?: string; disabled?: boolean; wide?: boolean; stack?: boolean }) {
  return (
    <div class="set-row" classList={{ disabled: props.disabled, wide: props.wide, stack: props.stack }}>
      <div class="set-row-text">
        <label class="set-label" for={props.for}>{typeof props.label === "string" ? tr(props.label) : props.label}</label>
        <Show when={props.hint}><div class="set-hint">{typeof props.hint === "string" ? tr(props.hint) : props.hint}</div></Show>
      </div>
      <Show when={props.children}><div class="set-row-control">{props.children}</div></Show>
    </div>
  );
}

export function Switch(props: { checked: boolean; onChange: (on: boolean) => void; label: string; disabled?: boolean; id?: string }) {
  return (
    <button id={props.id} type="button" role="switch" class="set-switch switch" classList={{ on: props.checked }} aria-checked={props.checked}
      aria-label={typeof props.label === "string" ? tr(props.label) : props.label} disabled={props.disabled} onClick={() => props.onChange(!props.checked)} />
  );
}

export function ToggleRow(props: { label: string; hint?: JSX.Element; checked: boolean; onChange: (on: boolean) => void; disabled?: boolean }) {
  const id = `t-${props.label.replace(/\W+/g, "-").toLowerCase()}`;
  return (
    <Row label={typeof props.label === "string" ? tr(props.label) : props.label} hint={typeof props.hint === "string" ? tr(props.hint) : props.hint} for={id} disabled={props.disabled}>
      <Switch id={id} checked={props.checked} onChange={props.onChange} label={typeof props.label === "string" ? tr(props.label) : props.label} disabled={props.disabled} />
    </Row>
  );
}

export function Check(props: { label: JSX.Element; checked: boolean; onChange: (on: boolean) => void; disabled?: boolean }) {
  return (
    <div class="set-check" classList={{ disabled: props.disabled }}>
      <label class="check">
        <input type="checkbox" checked={props.checked} disabled={props.disabled} onChange={(e) => props.onChange(e.currentTarget.checked)} />
        <span>{typeof props.label === "string" ? tr(props.label) : props.label}</span>
      </label>
    </div>
  );
}

export interface Option { value: string; label: string }

export function Select(props: { value: string; options: Option[]; onChange: (value: string) => void; label?: string; id?: string; disabled?: boolean }) {
  return (
    <select id={props.id} class="field set-select" value={props.value} aria-label={typeof props.label === "string" ? tr(props.label) : props.label} disabled={props.disabled}
      onChange={(e) => props.onChange(e.currentTarget.value)}>
      <For each={props.options}>{(o) => <option value={o.value} selected={o.value === props.value}>{tr(o.label)}</option>}</For>
    </select>
  );
}

/**
 * A slider with its value and a reset button that appears only after a
 * change and keeps its space while hidden (SettingsPage.h resetButton).
 * `onInput` follows the drag; `onCommit` stores the value once it settles;
 * `onEnd` (release, blur) ends a drag that settled where it started.
 */
export function Slider(props: {
  value: number; min: number; max: number; step: number; defaultValue: number; format: (v: number) => string;
  onInput: (v: number) => void; onCommit: (v: number) => void; onEnd?: () => void; disabled?: boolean; label: string; resetLabel: string;
}) {
  let input!: HTMLInputElement;
  return (
    <div class="set-slider" classList={{ disabled: props.disabled }}>
      <input ref={input} type="range" min={props.min} max={props.max} step={props.step} value={props.value} disabled={props.disabled} aria-label={typeof props.label === "string" ? tr(props.label) : props.label}
        aria-valuetext={props.format(props.value)}
        style={{ "--fill": `${((props.value - props.min) / (props.max - props.min)) * 100}%` }}
        onInput={(e) => props.onInput(Number(e.currentTarget.value))}
        onChange={(e) => props.onCommit(Number(e.currentTarget.value))}
        onPointerUp={() => props.onEnd?.()} onBlur={() => props.onEnd?.()} />
      <span class="set-slider-value tabular">{props.format(props.value)}</span>
      <button class="icon-btn small set-reset" classList={{ hidden: props.disabled || props.value === props.defaultValue }} title={tr(props.resetLabel)} aria-label={tr(props.resetLabel)}
        onClick={() => { input.focus(); props.onCommit(props.defaultValue); }}>
        <Icon name="rotate-ccw" size={13} />
      </button>
    </div>
  );
}

/** A status line from the mockup: muted, ok, warn, error, or busy with a spinner. */
export function Status(props: { kind?: "ok" | "warn" | "error" | "busy" | "muted"; children: JSX.Element }) {
  return (
    <div class={`set-status k-${props.kind ?? "muted"}`} role="status">
      <Show when={props.kind === "busy"}><span class="set-spinner" aria-hidden="true" /></Show>
      <Show when={props.kind === "ok"}><Icon name="check" size={13} /></Show>
      <Show when={props.kind === "warn"}><Icon name="triangle-alert" size={13} /></Show>
      <Show when={props.kind === "error"}><Icon name="circle-alert" size={13} /></Show>
      <span class="set-status-text">{props.children}</span>
    </div>
  );
}

/** A multi-line plain-text value, as Qt labels with \n show it. */
export function Lines(props: { text: string; class?: string }) {
  return <div class={`set-lines ${props.class ?? ""}`}><For each={props.text.split("\n")}>{(line) => <div>{line}</div>}</For></div>;
}

/** A number with its unit, clamped to its range (QDoubleSpinBox without buttons). */
export function NumberField(props: {
  value: number; min: number; max: number; step?: number; decimals?: number; suffix?: string;
  onChange: (v: number) => void; disabled?: boolean; id?: string; label?: string;
}) {
  const clamp = (v: number) => fieldValue(v, props.min, props.max, props.decimals);
  return (
    <span class="set-number" classList={{ disabled: props.disabled }}>
      <input id={props.id} class="field tabular" type="number" min={props.min} max={props.max} step={props.step ?? 1}
        value={props.value.toFixed(props.decimals ?? 0)} disabled={props.disabled} aria-label={typeof props.label === "string" ? tr(props.label) : props.label}
        onChange={(e) => {
          const kept = clamp(Number(e.currentTarget.value) || props.min);
          // Shown as kept even when the kept value did not change.
          e.currentTarget.value = kept.toFixed(props.decimals ?? 0);
          props.onChange(kept);
        }} />
      <Show when={props.suffix}><span class="set-suffix">{props.suffix}</span></Show>
    </span>
  );
}
