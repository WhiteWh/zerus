import { createEffect, createRoot, createSignal } from "solid-js";
import * as bridge from "../bridge";
import { notify } from "./actions";
import { composer } from "./composer";
import { trText } from "../text";
import type { ButtonView, ColdAnswer, QuestionEdit, QuestionReply, QuestionView } from "../types";

// The shown question card (Qt's QuestionCard): its review delay, its replies
// and its cold cache question. Rust holds the card, its answers and their
// draft; the webview sends each edit at once with the epoch its fields show
// (no debounce: the epoch is shared by every card, and Qt captures each key).

/** Qt's review delay before an approval can be granted, and its timer (QuestionCard.cpp:237-246). */
export const REVIEW_MS = 3000;
export const TICK_MS = 100;

export interface QuestionPort {
  edit(card: string, epoch: number, edit: QuestionEdit): Promise<void>;
  page(card: string, page: number): Promise<void>;
  navigate(direction: number): Promise<void>;
  allowRetry(card: string): Promise<void>;
  submit(card: string): Promise<QuestionReply>;
  decide(card: string, allow: boolean): Promise<QuestionReply>;
  skip(card: string): Promise<QuestionReply>;
  cold(token: number, proceed: boolean): Promise<QuestionReply>;
  notify(text: string): void;
}

export interface Clock { now(): number; every(ms: number, f: () => void): () => void }

const browserClock: Clock = {
  now: () => performance.now(),
  every: (ms, f) => { const id = setInterval(f, ms); return () => clearInterval(id); },
};

export function createQuestions(port: QuestionPort, clock: Clock = browserClock) {
  const [view, setView] = createSignal<QuestionView | undefined>();
  const [left, setLeft] = createSignal(REVIEW_MS / 1000);
  const [dialog, setDialog] = createSignal<{ token: number; prompt: ColdAnswer; card: string } | undefined>();
  let active = false;
  let started: number | undefined;
  let shown = "";
  let stop: (() => void) | undefined;
  let coldEpoch = 0;
  const secondsLeft = (elapsed: number) => Math.ceil(Math.max(0, REVIEW_MS - elapsed) / 1000);

  /** A gone card or view cancels even a cold reply still crossing the bridge. */
  function cancelCold() {
    coldEpoch++;
    const d = dialog();
    setDialog(undefined);
    if (d) void port.cold(d.token, false).catch(() => {});
  }

  /** The count runs only while the card is on screen, the window is active and the card is available. */
  function tick() {
    const v = view();
    if (!v?.view.review) return;
    if (!active || !v.view.available) { started = undefined; setLeft(secondsLeft(0)); return; }
    started ??= clock.now();
    setLeft(secondsLeft(clock.now() - started));
  }

  /** A new card or a rebuilt one (its revision) starts the review again; a grown epoch alone does not. Only a review card ticks. */
  function apply(next: QuestionView | null | undefined) {
    // A cold question goes with its card: Cancel, nothing sent (the hub drops it too).
    if (next?.card !== view()?.card) cancelCold();
    const id = next ? `${next.card}\n${next.revision}` : "";
    if (id !== shown) { shown = id; started = undefined; setLeft(secondsLeft(0)); }
    setView(next ?? undefined);
    const review = !!next?.view.review;
    if (review && !stop) stop = clock.every(TICK_MS, tick);
    if (!review && stop) { stop(); stop = undefined; }
    tick();
  }

  const reviewing = (b: ButtonView) => b.review_label !== null && left() > 0;
  const label = (b: ButtonView) => (reviewing(b) ? `${trText(b.review_label!)} (${left()})` : trText(b.label));
  const enabled = (b: ButtonView) => b.visible && b.enabled && !reviewing(b);
  const reply = (r: QuestionReply, card: string, at: number) => {
    if (r.kind === "cold") {
      if (at !== coldEpoch || view()?.card !== card) void port.cold(r.token, false).catch(() => {});
      else setDialog({ token: r.token, prompt: r.prompt, card });
    }
    else if (r.kind === "notice") port.notify(r.text);
  };
  const card = () => view()?.card;

  async function submit() {
    const v = view();
    const at = coldEpoch;
    if (v && enabled(v.view.submit)) reply(await port.submit(v.card), v.card, at);
  }

  return {
    view, left, dialog, apply, label, enabled, submit, cancelCold,
    setActive(on: boolean) { active = on; tick(); },
    /** Every edit goes at once, naming the epoch the fields show; Rust drops one of an older epoch. */
    edit(e: QuestionEdit) { const v = view(); if (v) void port.edit(v.card, v.epoch, e); },
    async page(i: number) { const c = card(); if (c !== undefined) await port.page(c, i); },
    async navigate(direction: number) { await port.navigate(direction); },
    async allowRetry() { const c = card(); if (c !== undefined) await port.allowRetry(c); },
    /** Enter on an option or the answer line: Submit when it shows, never on auto-repeat (898-914). */
    async enter(repeat: boolean) { if (!repeat && view()?.view.submit.visible) await submit(); },
    async decide(allow: boolean) {
      const v = view();
      const at = coldEpoch;
      if (v && enabled(allow ? v.view.approve : v.view.deny)) reply(await port.decide(v.card, allow), v.card, at);
    },
    async skip() { const v = view(), at = coldEpoch; if (v && enabled(v.view.skip)) reply(await port.skip(v.card), v.card, at); },
    async answerCold(proceed: boolean) {
      const d = dialog();
      if (!d) return;
      const at = coldEpoch;
      setDialog(undefined);
      reply(await port.cold(d.token, proceed), d.card, at);
    },
  };
}

export type QuestionsStore = ReturnType<typeof createQuestions>;

/** The app's card follows the composer's accepted view, so a view of another composer never shows its card. */
export const questions = createRoot(() => {
  // The bridge is read at each call, not when this module loads.
  const q = createQuestions({
    edit: (card, epoch, edit) => bridge.questionEdit(card, epoch, edit),
    page: (card, page) => bridge.questionPage(card, page),
    navigate: (direction) => bridge.questionNavigate(direction),
    allowRetry: (card) => bridge.questionAllowRetry(card),
    submit: (card) => bridge.questionSubmit(card),
    decide: (card, allow) => bridge.questionDecide(card, allow),
    skip: (card) => bridge.questionSkip(card),
    cold: (token, proceed) => bridge.questionCold(token, proceed),
    notify: (text) => notify("error", text, true),
  });
  createEffect(() => q.apply(composer.view()?.question));
  return q;
});
