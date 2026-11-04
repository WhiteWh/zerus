import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { Icon } from "../lib/icons";
import { MachineLabel } from "../components/MachineLabel";
import { createConflicts, pageRange, type Review } from "./swarm-conflicts";
import type { SwarmView } from "./project-swarm";
import "../styles/swarm-conflicts.css";
import { tr } from "../../i18n";

export default function SwarmConflicts(props: { view(): SwarmView | undefined; working(): boolean; changed(): void; resolve(review: string, confirmed: boolean): Promise<boolean>; report(error: string): void }) {
  const [submitting, setSubmitting] = createSignal(false);
  const ready = () => !submitting() && !!props.view() && !props.working() && !props.view()!.busy && (props.view()!.can_sync || !["join", "sync", "resolve", "reconcile"].includes(props.view()!.mode));
  const state = createConflicts(invoke, () => props.view()!.id, ready, props.changed);
  const [confirm, setConfirm] = createSignal<{ id: string; choice: number; text: string; origin: HTMLElement }>();
  const busy = () => submitting() || state.busy() || props.working() || !!props.view()?.busy || state.consumed();
  let stamp = "", requestedConsumed = false, leftPrev!: HTMLButtonElement, leftNext!: HTMLButtonElement, rightPrev!: HTMLButtonElement, rightNext!: HTMLButtonElement;
  let focus: { button: HTMLButtonElement; other: () => HTMLButtonElement } | undefined;
  createEffect(() => { if (!busy() && focus) { const target = focus; focus = undefined; queueMicrotask(() => { if (target.button.isConnected) (target.button.disabled ? target.other() : target.button)?.focus(); }); } });
  createEffect(() => {
    const frame = props.view(), next = `${frame?.id}:${frame?.catalog?.revision ?? 0}`;
    if (!state.consumed()) requestedConsumed = false;
    if (ready() && (next !== stamp || state.consumed() && !requestedConsumed)) { stamp = next; requestedConsumed = state.consumed(); state.changed(); }
  });
  createEffect(() => props.report(state.error()));
  createEffect(() => {
    const c = confirm(), current = state.review();
    if (c && (current?.id !== c.id || current.chosen !== c.choice || state.consumed())) setConfirm(undefined);
  });
  onCleanup(state.dispose);
  const save = async (review: Review, confirmed: boolean) => {
    if (busy() || !props.view()?.can_sync || review.chosen === null) return;
    setSubmitting(true); setConfirm(undefined); state.consume();
    try { await props.resolve(review.id, confirmed); } finally { setSubmitting(false); }
  };
  const resolve = (review: Review) => {
    if (!review.selection || review.chosen === null || busy()) return;
    if (review.selection.confirmation) setConfirm({ id: review.id, choice: review.chosen, text: `${review.summary.subject}\n\n${review.selection.outcome}`, origin: document.activeElement as HTMLElement });
    else void save(review, false);
  };
  const turn = async (button: HTMLButtonElement, other: () => HTMLButtonElement, work: () => Promise<boolean>) => {
    focus = { button, other }; await work();
  };
  return <>
    <Show when={state.list()?.total === 0}><p class="sw-msg su-pre">{tr("No conflicting changes.\nChanges from connected computers are merged automatically.")}</p></Show>
    <Show when={state.list()?.total}>
      <div class="sw-page">
        <p class="muted">{tr("These changes need your decision. Review one item at a time; each choice is shared with the swarm.")}</p>
        <div class="sw-conflicts grow">
          <div class="sw-cside"><ul class="sw-clist" role="listbox" aria-label={tr("Conflicts")}><For each={state.list()?.rows}>{(row) => <li role="option" tabindex="0" aria-selected={state.review()?.summary.id === row.id} classList={{ on: state.review()?.summary.id === row.id }} onClick={() => !busy() && void state.open(row.id)} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); if (!busy()) void state.open(row.id); } }}><b>{row.setting}</b><span class="muted ell" title={row.subject}>{row.subject}</span></li>}</For></ul>
            <Show when={(state.list()?.total ?? 0) > 64}><div class="row sw-cpager">
              <button ref={leftPrev} class="btn small" aria-label={tr("Previous page of conflicts")} title={tr("Previous page of conflicts")} disabled={busy() || state.list()!.offset === 0} onClick={(e) => void turn(e.currentTarget, () => leftNext, () => state.page(Math.floor(state.list()!.offset / 64) - 1))}><Icon name="chevron-left" size={14} /></button>
              <span class="muted tabular grow" title={`Conflicts ${pageRange(state.list()!)}`}>{pageRange(state.list()!)}</span>
              <button ref={leftNext} class="btn small" aria-label={tr("Next page of conflicts")} title={tr("Next page of conflicts")} disabled={busy() || state.list()!.offset + state.list()!.rows.length >= state.list()!.total} onClick={(e) => void turn(e.currentTarget, () => leftPrev, () => state.page(Math.floor(state.list()!.offset / 64) + 1))}><Icon name="chevron-right" size={14} /></button>
            </div></Show>
          </div>
          <Show when={state.review()}>{(review) => <section class="sw-review">
            <h3 class="section-title">{review().summary.setting}</h3><div class="row"><span class="grow">{review().summary.subject}</span><Show when={review().summary.computer}>{(computer) => <MachineLabel alias={computer()} class="mbadge" />}</Show></div>
            <p class="muted su-pre">{review().explanation}</p>
            <table class="su-table sw-choices"><thead><tr><th>{tr("Value to keep")}</th><th>{tr("Changed on")}</th><th>{tr("Display")}</th></tr></thead><tbody><For each={review().choices.rows}>{(choice) => <tr classList={{ on: review().chosen === choice.index }} tabindex="0" aria-selected={review().chosen === choice.index} onClick={() => !busy() && void state.choose(choice.index)} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); if (!busy()) void state.choose(choice.index); } }}>
              <td class="su-pre" classList={{ danger: choice.removal }}>{choice.text}</td><td><For each={choice.origins}>{(origin) => <div><Show when={origin.computer} fallback={origin.text}>{(computer) => <><MachineLabel alias={computer()} class="mbadge" /><Show when={origin.local}><span class="hint"> {tr("this computer")}</span></Show></>}</Show></div>}</For></td><td class="hint">{choice.shown ? tr("Shown now") : ""}</td>
            </tr>}</For></tbody></table>
            <Show when={review().choices.total > 32}><div class="row sw-vpager"><span class="muted tabular">{tr("%1 values", pageRange(review().choices))}</span><span class="grow" />
              <button ref={rightPrev} class="btn small" aria-label={tr("Previous page of values")} disabled={busy() || review().choices.offset === 0} onClick={(e) => void turn(e.currentTarget, () => rightNext, () => state.values(Math.floor(review().choices.offset / 32) - 1))}><Icon name="chevron-left" size={14} />{tr("Previous")}</button>
              <button ref={rightNext} class="btn small" aria-label={tr("Next page of values")} disabled={busy() || review().choices.offset + review().choices.rows.length >= review().choices.total} onClick={(e) => void turn(e.currentTarget, () => rightPrev, () => state.values(Math.floor(review().choices.offset / 32) + 1))}>{tr("Next")}<Icon name="chevron-right" size={14} /></button>
            </div></Show>
            <p class="hint su-pre sw-outcome">{review().selection?.outcome ?? tr("Select a row to see what will change. Closing this window leaves the conflict for later.")}</p>
            <div class="row"><span class="grow" /><button class="btn primary" disabled={busy() || !props.view()?.can_sync || !review().selection} onClick={() => resolve(review())}>{review().selection?.action ?? tr("Choose a value above")}</button></div>
          </section>}</Show>
        </div>
      </div>
    </Show>
    <Show when={confirm()}>{(c) => <Confirm text={c().text} origin={c().origin} busy={() => state.busy() || props.working()} close={() => setConfirm(undefined)} apply={() => { const r = state.review(); if (r?.id === c().id && r.chosen === c().choice) void save(r, true); }} />}</Show>
  </>;
}
function Confirm(props: { text: string; origin: HTMLElement; busy(): boolean; close(): void; apply(): void }) {
  let dialog!: HTMLDialogElement;
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLButtonElement>(".dlg-foot [autofocus]")?.focus()); });
  onCleanup(() => { dialog.close(); queueMicrotask(() => { if (props.origin?.isConnected) props.origin.focus(); }); });
  return <dialog ref={dialog} class="dlg" style={{ width: "480px" }} aria-labelledby="swarm-confirm-title" onCancel={(e) => { e.preventDefault(); e.stopPropagation(); props.close(); }}>
    <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="swarm-confirm-title">{tr("Confirm catalog change")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={props.close}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body"><p class="confirm-body su-pre">{props.text}</p></div><div class="dlg-foot"><span class="grow" /><button type="button" class="btn solid-danger" disabled={props.busy()} onClick={props.apply}>{tr("Apply")}</button><button type="button" class="btn primary" autofocus onClick={props.close}>{tr("Cancel")}</button></div>
  </dialog>;
}
