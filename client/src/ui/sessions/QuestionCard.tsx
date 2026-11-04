import { Index, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount, untrack } from "solid-js";
import { tr } from "../../i18n";
import { trText } from "../../text";
import { askedTimeOf, zonedTimeOf } from "../lib/dates";
import { Icon } from "../lib/icons";
import { questions } from "../../store/questions";
import { terminal } from "../../store/terminal";
import { setDetailTab } from "../../store/ui";
import { selected } from "../../store/ui";
import { nativeBinding, nativeRoute } from "./native-entry";
import { Body } from "./Activity";
import type { FormView, OptionView } from "../../types";

/** The shown question card (Qt's QuestionCard), between Activity and the composer (SessionsWindow.cpp:918, 1030). */
export function QuestionCard() {
  // Rebuilt for a new card or a rebuilt one only: typing never remounts the fields.
  const ident = createMemo(() => { const q = questions.view(); return q ? `${q.card}\n${q.revision}` : undefined; });
  // A card rises in only when a new question arrives in a session already open.
  // Opening a session, or rebuilding the same card, paints it in place.
  let openedAt = performance.now(), shownCard: string | undefined;
  createEffect(on(() => selected()?.key, () => { openedAt = performance.now(); shownCard = undefined; }, { defer: true }));
  const arrived = () => {
    const card = untrack(() => questions.view()?.card);
    const fresh = card !== shownCard && performance.now() - openedAt > 1500;
    shownCard = card;
    return fresh;
  };
  return (
    <>
      <Show when={ident()} keyed>{(_ident) => <Card arrived={arrived()} />}</Show>
      <ColdAnswerDialog />
    </>
  );
}

/**
 * Rust's answers come back when the epoch grows; `apply` puts them into a
 * field the user may have changed. An echo of the same epoch (every edit's
 * own view) leaves the field alone.
 */
function onEpoch(apply: () => void) {
  const epoch = createMemo(() => questions.view()?.epoch);
  let last = untrack(epoch);
  createEffect(() => {
    const now = epoch();
    if (now !== undefined && last !== undefined && now > last) untrack(apply);
    last = now;
  });
}

/** Enter or Space held down never acts again, and Enter on a button does nothing: Qt's card buttons are not auto-default (QuestionCard.cpp:907). */
function guardKeys(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229 || (e.key !== "Enter" && e.key !== " ")) return;
  if (e.repeat || (e.key === "Enter" && e.target instanceof HTMLButtonElement)) { e.preventDefault(); e.stopPropagation(); }
}

function Card(props: { arrived: boolean }) {
  const openNative = nativeRoute(selected());
  const q = () => questions.view()!;
  const c = () => q().view;
  // Tabs stay browsable while the card is locked; the draft keeps its page (QuestionCard.cpp:249-255).
  const [browse, setBrowse] = createSignal<number>();
  createEffect(() => { if (!c().locked) setBrowse(undefined); });
  const page = () => (c().locked ? browse() ?? c().page : c().page);
  const formId = () => c().forms[page()]?.id;
  const choose = (i: number) => { if (c().locked) setBrowse(i); else void questions.page(i); };
  let el!: HTMLDivElement;
  onMount(() => {
    // The review delay counts only while the card is on screen and the window is active (237-246).
    let visible = false;
    const update = () => questions.setActive(visible && document.visibilityState === "visible" && document.hasFocus());
    const observer = new IntersectionObserver((entries) => { visible = entries.some((e) => e.isIntersecting); update(); });
    observer.observe(el);
    window.addEventListener("focus", update);
    window.addEventListener("blur", update);
    document.addEventListener("visibilitychange", update);
    onCleanup(() => {
      observer.disconnect();
      window.removeEventListener("focus", update);
      window.removeEventListener("blur", update);
      document.removeEventListener("visibilitychange", update);
      questions.setActive(false);
    });
  });
  const native = () => q().native_ui;
  // Rust's "Asked" text is the fallback; the webview dates it in the interface language.
  const asked = () => { const at = c().created_at; return at > 0 && at < 253_402_300_800 ? new Date(at * 1000) : undefined; };
  const terminalTip = () => (native() ? undefined : terminal.texts()?.available ? undefined : terminal.texts()?.reason);
  const onKey = (e: KeyboardEvent) => {
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key !== "Enter" || e.shiftKey || e.ctrlKey || e.altKey || e.metaKey) return;
    e.preventDefault();
    void questions.enter(e.repeat);
  };
  // The owner's "needs attention" card: icon, heading and Asked on top; counters below when there are any.
  return (
    <div class="cmp-col"><div class="qcard" classList={{ arrived: props.arrived }} ref={el} role="group" aria-label={trText(c().heading)} onKeyDown={guardKeys}>
      <div class="qcard-head">
        <Icon name="circle-question-mark" size={16} class="warn" />
        <b class="qcard-title">{trText(c().heading)}</b><span class="grow" />
        <Show when={q().asked}><span class="qcard-dim" title={asked() ? zonedTimeOf(asked()!) : q().asked_tip}>{asked() ? tr("Asked %1", askedTimeOf(asked()!)) : q().asked}</span></Show>
      </div>
      <Show when={c().progress || c().pending || c().nav}>
        <div class="qcard-meta">
          <Show when={c().progress}>{(p) => <span class="qcard-dim">{trText(p())}</span>}</Show>
          <span class="grow" />
          <Show when={c().pending}>{(p) => <span class="qcard-dim" title={trText(c().pending_tip)}>{trText(p())}</span>}</Show>
          <Show when={c().nav}>{(nav) => (
            <>
              <button type="button" class="icon-btn small" aria-label={tr("Previous queued question")} title={trText(nav().previous_tip)} disabled={!nav().previous}
                onClick={() => void questions.navigate(-1)}><Icon name="chevron-left" size={12} /></button>
              <button type="button" class="icon-btn small" aria-label={tr("Next queued question")} title={trText(nav().next_tip)} disabled={!nav().next}
                onClick={() => void questions.navigate(1)}><Icon name="chevron-right" size={12} /></button>
            </>
          )}</Show>
        </div>
      </Show>
      <Show when={c().tabs_visible}>
        <div class="qcard-tabs tabs" role="tablist">
          <Index each={c().forms}>{(f, i) => (
            <button type="button" class="tab" role="tab" classList={{ on: i === page() }} aria-selected={i === page()}
              onClick={() => choose(i)}>{trText(f().tab)}</button>
          )}</Index>
        </div>
      </Show>
      <Show when={formId()} keyed>{(id) => <Form id={id} onKey={onKey} />}</Show>
      {/* The hint on the left, the actions on the right; they wrap under it when the column is narrow. */}
      <div class="qcard-actions">
        <p class="qcard-status" classList={{ error: c().status_error }} title={trText(c().status)} role="status" aria-live="polite">{trText(c().status)}</p>
        <button type="button" class="btn small ghost" disabled={native() ? !nativeBinding(selected()) : !terminal.texts()?.available} title={terminalTip()}
          onClick={() => { if (native()) openNative(); else { terminal.opened(); setDetailTab("terminal"); } }}>{trText(q().terminal_label)}</button>
        <Show when={c().skip.visible}><button type="button" class="btn small ghost" title={trText(c().skip_tip)} disabled={!questions.enabled(c().skip)} onClick={() => void questions.skip()}>{trText(c().skip.label)}</button></Show>
        <Show when={c().retry}><button type="button" class="btn small ghost" title={trText(c().retry_tip)} onClick={() => void questions.allowRetry()}>{tr("Allow retry")}</button></Show>
        <Show when={c().previous.visible}><button type="button" class="btn small ghost" disabled={!c().previous.enabled} onClick={() => void questions.page(c().page - 1)}>{trText(c().previous.label)}</button></Show>
        <Show when={c().next.visible}><button type="button" class="btn small ghost accent" disabled={!c().next.enabled} onClick={() => void questions.page(c().page + 1)}>{trText(c().next.label)}</button></Show>
        <Show when={c().submit.visible}><button type="button" class="btn small primary" disabled={!questions.enabled(c().submit)} onClick={() => void questions.submit()}>{questions.label(c().submit)}</button></Show>
        <Show when={c().deny.visible}><button type="button" class="btn small ghost" disabled={!questions.enabled(c().deny)} onClick={() => void questions.decide(false)}>{trText(c().deny.label)}</button></Show>
        <Show when={c().approve.visible}><button type="button" class="btn small primary" disabled={!questions.enabled(c().approve)} onClick={() => void questions.decide(true)}>{questions.label(c().approve)}</button></Show>
      </div>
    </div></div>
  );
}

function Form(props: { id: string; onKey: (e: KeyboardEvent) => void }) {
  const f = () => questions.view()?.view.forms.find((form) => form.id === props.id);
  const group = `qcard-${props.id}`;
  return (
    <Show when={f()}>{(form) => (
      <div class="qcard-page">
        <div class="qcard-scroll">
          <Show when={form().markdown} fallback={<p class="qcard-prompt wrap-pre">{form().prompt}</p>}><div class="qcard-prompt"><Body text={form().prompt} /></div></Show>
          <Show when={form().body}>
            <Show when={form().markdown} fallback={<p class="qcard-body wrap-pre" classList={{ mono: form().monospace_body }}>{form().body}</p>}><div class="qcard-body"><Body text={form().body} /></div></Show>
          </Show>
          <Show when={form().mode_visible}><p class="qcard-mode">{trText(form().mode)}</p></Show>
          <div class="qcard-options">
            <Index each={form().options}>{(o) => <Choice form={form()} id={o().id} group={group} onKey={props.onKey} />}</Index>
            {/* Other keeps its own row, and the answer line opens inside it once Other is chosen. */}
            <Show when={form().other}>
              <div class="qother" classList={{ on: !!form().other?.checked, off: !form().other?.enabled }}>
                <Choice form={form()} group={group} other onKey={props.onKey} />
                <Show when={form().text?.visible}><AnswerLine id={props.id} onKey={props.onKey} /></Show>
              </div>
            </Show>
          </div>
        </div>
        <Show when={!form().other && form().text?.visible}><AnswerLine id={props.id} onKey={props.onKey} /></Show>
      </div>
    )}</Show>
  );
}

/** The free-text line: the user's text until Rust hands its own back with a grown epoch. */
function AnswerLine(props: { id: string; onKey: (e: KeyboardEvent) => void }) {
  const t = () => questions.view()?.view.forms.find((form) => form.id === props.id)?.text;
  let input!: HTMLInputElement;
  onEpoch(() => { const value = t()?.value ?? ""; if (input && input.value !== value) input.value = value; });
  return (
    <input ref={input} class="qcard-text" type="text" placeholder={t() ? trText(t()!.placeholder) : undefined} aria-label={t() ? trText(t()!.accessible_name) : undefined} maxLength={t()?.max_units}
      disabled={!t()?.enabled} value={untrack(() => t()?.value ?? "")}
      onInput={(e) => {
        const el = e.currentTarget;
        const max = t()?.max_chars ?? 0;
        // OpenCode counts its bound in characters, as Qt's CharacterLimit does (785-786).
        if (max > 0 && [...el.value].length > max) el.value = [...el.value].slice(0, max).join("");
        questions.edit({ kind: "text", item: props.id, text: el.value });
      }}
      onKeyDown={props.onKey} />
  );
}

function Choice(props: { form: FormView; id?: string; group: string; other?: boolean; onKey: (e: KeyboardEvent) => void }) {
  const o = (): OptionView | undefined => {
    const form = questions.view()?.view.forms.find((f) => f.id === props.form.id);
    return props.other ? form?.other ?? undefined : form?.options.find((option) => option.id === props.id);
  };
  let input: HTMLInputElement | undefined;
  onEpoch(() => { const checked = !!o()?.checked; if (input && input.checked !== checked) input.checked = checked; });
  const toggle = (checked: boolean) => questions.edit(props.other
    ? { kind: "other", item: props.form.id, checked }
    : { kind: "option", item: props.form.id, option: props.id ?? "", checked });
  return (
    <Show when={o()?.visible}>
      <label class="qchoice" classList={{ on: !!o()?.checked, off: !o()?.enabled }}>
        <input ref={input} type={props.form.multi ? "checkbox" : "radio"} name={props.group} checked={!!o()?.checked} disabled={!o()?.enabled}
          onChange={(e) => toggle(e.currentTarget.checked)} onKeyDown={props.onKey} />
        <span class="qchoice-copy">
          <span class="qchoice-title">{o() ? trText(o()!.label) : ""}</span>
          <Show when={o() && trText(o()!.description)}>
            <Show when={o()?.markdown} fallback={<span class="qchoice-desc wrap-pre">{o() ? trText(o()!.description) : ""}</span>}><span class="qchoice-desc"><Body text={o() ? trText(o()!.description) : ""} /></span></Show>
          </Show>
        </span>
      </label>
    </Show>
  );
}

/** "Submit with a cold cache?": Cancel is the default and Escape (SessionsWindow.cpp:5111-5131). */
function ColdAnswerDialog() {
  let el: HTMLDialogElement | undefined;
  createEffect(() => {
    const d = questions.dialog();
    if (!el) return;
    if (!d) { if (el.open) el.close(); return; }
    if (!el.open) el.showModal();
    queueMicrotask(() => el?.querySelector<HTMLElement>("[data-initial]")?.focus());
  });
  // A dialog that goes unanswered answers Cancel, so the hub holds no question nobody can answer.
  onCleanup(() => questions.cancelCold());
  return (
    <dialog class="dlg" ref={el} role="alertdialog" aria-labelledby="qcard-cold-title" aria-describedby="qcard-cold-text" style={{ width: "460px" }}
      onClose={() => { if (questions.dialog()) void questions.answerCold(false); }}>
      <Show when={questions.dialog()}>{(d) => (
        <>
          <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="qcard-cold-title">{d().prompt.title}</h2></div>
          <div class="dlg-body"><div class="confirm-body wrap-pre" id="qcard-cold-text">{d().prompt.text}</div></div>
          <div class="dlg-foot">
            <span class="spacer" />
            <button type="button" class="btn primary" data-initial autofocus onClick={() => void questions.answerCold(false)}>{tr("Cancel")}</button>
            <button type="button" class="btn" onClick={() => void questions.answerCold(true)}>{d().prompt.proceed}</button>
          </div>
        </>
      )}</Show>
    </dialog>
  );
}
