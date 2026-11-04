import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";
import {
  COMMANDS, assignable, command, comboFromEvent, conflicts, customized, keys, nativeText, overlaps, platform, reset,
  resetAll, setKeys, terminalRule, terminalSafe, type Sequence,
} from "../../../store/shortcuts";
import { SectionHead } from "./ui";
import { tr, trNoop } from "../../../i18n";

// Settings > Keyboard (tray/src/KeyboardSettings.cpp): every command by group
// with its two chords, Reset and Reset all, and the change dialog with Qt's
// assignability rules and reassignment of conflicting chords. Chords appear
// here and nowhere else in the client.

/** QLocale().createSeparatedList for English: "A", "A and B", "A, B and C". */
export function separatedList(names: string[]): string {
  if (names.length < 2) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

const groups = () => {
  const order: string[] = [];
  for (const c of COMMANDS) if (!order.includes(c.group)) order.push(c.group);
  return order.map((group) => ({ group, commands: COMMANDS.filter((c) => c.group === group) }));
};

/** A chord as the table shows it: native text, with ✓ when it works in Terminal. */
function Chord(props: { id: string; slot: number }) {
  const sequence = () => keys(props.id)[props.slot];
  const works = () => command(props.id)?.scope === "window" && !!sequence() && terminalSafe(sequence()![0]);
  return (
    <Show when={sequence()}>
      {(s) => <span title={works() ? tr("Works in Terminal") : undefined}>{nativeText(s())}{works() ? " ✓" : ""}</span>}
    </Show>
  );
}

export function KeyboardSection() {
  const [selected, setSelected] = createSignal<string | undefined>();
  const [editing, setEditing] = createSignal<{ id: string; slot: number } | undefined>();
  const [confirmAll, setConfirmAll] = createSignal(false);
  const list = createMemo(groups);
  // One tab stop: the selected row (the first until one is); arrows move it.
  const order = createMemo(() => list().flatMap((g) => g.commands.map((c) => c.id)));
  const current = () => selected() ?? order()[0];
  const rows = new Map<string, HTMLTableRowElement>();
  const move = (id: string | undefined) => { if (!id) return; setSelected(id); rows.get(id)?.focus(); };
  const rowKey = (e: KeyboardEvent, id: string) => {
    const ids = order();
    const at = ids.indexOf(id);
    const to = e.key === "ArrowDown" ? ids[Math.min(at + 1, ids.length - 1)]
      : e.key === "ArrowUp" ? ids[Math.max(at - 1, 0)]
      : e.key === "Home" ? ids[0] : e.key === "End" ? ids[ids.length - 1] : undefined;
    if (e.key === "Enter") { e.preventDefault(); setEditing({ id, slot: 0 }); }
    else if (to) { e.preventDefault(); move(to); }
  };
  return (
    <>
      <SectionHead title={trNoop("Keyboard")} />
      <div class="set-table-wrap kb-wrap">
        <table class="set-table kb-table" role="grid" aria-label={tr("Keyboard shortcuts")}>
          <thead><tr><th>{tr("Command")}</th><th>{tr("Shortcut")}</th><th>{tr("Alternate")}</th></tr></thead>
          <tbody>
            <For each={list()}>
              {(g) => (
                <>
                  <tr class="set-table-group"><td colSpan={3}>{tr(g.group)}</td></tr>
                  <For each={g.commands}>
                    {(c) => (
                      <tr class="kb-row" ref={(el) => rows.set(c.id, el)} classList={{ on: selected() === c.id }}
                        tabindex={current() === c.id ? 0 : -1} aria-selected={selected() === c.id}
                        onClick={() => setSelected(c.id)} onFocus={() => setSelected(c.id)} onKeyDown={(e) => rowKey(e, c.id)}>
                        <td class="kb-cmd" classList={{ custom: customized(c.id) }} onDblClick={() => setEditing({ id: c.id, slot: 0 })}>{tr(c.title)}</td>
                        <td class="kb-chord" onDblClick={() => setEditing({ id: c.id, slot: 0 })}><Chord id={c.id} slot={0} /></td>
                        <td class="kb-chord" onDblClick={() => setEditing({ id: c.id, slot: 1 })}><Chord id={c.id} slot={1} /></td>
                      </tr>
                    )}
                  </For>
                </>
              )}
            </For>
          </tbody>
        </table>
      </div>
      <div class="set-actions">
        <button class="btn" disabled={!selected()} title={tr("Restore the default chords of the selected command")} onClick={() => { const id = selected(); if (id) reset(id); }}>{tr("Reset")}</button>
        <button class="btn" onClick={() => setConfirmAll(true)}>{tr("Reset all")}</button>
      </div>
      <p class="set-foot">{terminalRule()} {tr("Chords marked ✓ work in Terminal.")}</p>
      <p class="set-foot">{tr("Double-click a shortcut, or press Enter on it, to change it.")}</p>
      <Show when={editing()}>{(e) => <ChangeDialog id={e().id} slot={e().slot} close={() => setEditing(undefined)} />}</Show>
      <Show when={confirmAll()}>
        <Question title={tr("Reset all shortcuts")} text={tr("Restore the default chords of every command?")}
          answer={(yes) => { setConfirmAll(false); if (yes) resetAll(); }} />
      </Show>
    </>
  );
}

/** A modal dialog closed before it unmounts, so the focus returns to where it was. */
function modal(open: (dialog: HTMLDialogElement) => void) {
  let dialog: HTMLDialogElement | undefined;
  const close = () => { if (dialog?.open) dialog.close(); };
  onCleanup(close);
  return { ref: (el: HTMLDialogElement) => { dialog = el; queueMicrotask(() => open(el)); }, close };
}

/** QMessageBox::question: Yes or No. */
function Question(props: { title: string; text: string; answer: (yes: boolean) => void }) {
  const dialog = modal((el) => el.showModal());
  const answer = (yes: boolean) => { dialog.close(); props.answer(yes); };
  return (
    <dialog class="dlg kb-question" ref={dialog.ref} aria-labelledby="kb-question-title"
      onCancel={(e) => { e.preventDefault(); answer(false); }}>
      <div class="dlg-head"><h2 id="kb-question-title">{props.title}</h2></div>
      <div class="dlg-body">{props.text}</div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn" onClick={() => answer(true)}>{tr("Yes")}</button>
        <button type="button" class="btn primary" autofocus onClick={() => answer(false)}>{tr("No")}</button>
      </div>
    </dialog>
  );
}

/** Change shortcut (KeyboardSettings::editChord). */
function ChangeDialog(props: { id: string; slot: number; close: () => void }) {
  const title = tr(command(props.id)?.title ?? props.id);
  const [recorded, setRecorded] = createSignal<Sequence>(keys(props.id)[props.slot] ?? []);
  const [reason, setReason] = createSignal("");
  const [asking, setAsking] = createSignal<string[] | undefined>();
  let field!: HTMLDivElement;
  const dialog = modal((el) => { el.showModal(); field.focus(); });
  const close = () => { dialog.close(); props.close(); };

  // QKeySequenceEdit with at most two chords: the first key replaces the
  // chord shown, a second follows it, and recording finishes at two chords,
  // a second after any key is released (macOS sends no release for a key
  // let go while ⌘ is held, so the modifier's counts), or when the field
  // loses the focus.
  const [recording, setRecording] = createSignal(false);
  let release: ReturnType<typeof setTimeout> | undefined;
  const stopTimer = () => { if (release !== undefined) clearTimeout(release); release = undefined; };
  const finish = () => { stopTimer(); setRecording(false); };
  onCleanup(stopTimer);
  const record = (e: KeyboardEvent) => {
    if (e.key === "Tab" && !e.ctrlKey && !e.metaKey && !e.altKey) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.repeat) return;
    const combo = comboFromEvent(e);
    if (!combo) return;
    stopTimer();
    const sequence = recording() ? [...recorded(), combo] : [combo];
    setRecorded(sequence);
    setReason("");
    if (sequence.length >= 2) finish(); else setRecording(true);
  };
  const released = () => {
    if (!recording()) return;
    stopTimer();
    release = setTimeout(finish, 1000);
  };

  const apply = () => {
    const sequence = recorded();
    let current = keys(props.id);
    if (!sequence.length) {
      if (props.slot < current.length) current = current.filter((_, i) => i !== props.slot);
    } else if (props.slot < current.length) {
      // The other slot goes when it is the same chord or a prefix either way.
      current = current.map((key, i) => (i === props.slot ? sequence : key)).filter((key, i) => i === props.slot || !overlaps(key, sequence));
    } else {
      current = [...current.filter((key) => !overlaps(key, sequence)), sequence];
    }
    setKeys(props.id, current);
    close();
  };

  const accept = () => {
    const sequence = recorded();
    const why = sequence.length ? assignable(sequence, platform) : undefined;
    if (why) { setReason(why); return; }
    const others = conflicts(props.id, sequence);
    if (others.length) { setAsking(others); return; }
    apply();
  };

  // Every other command whose chord equals the sequence or is a prefix
  // either way loses that chord, after one question naming them all.
  const reassign = (yes: boolean) => {
    const others = asking() ?? [];
    setAsking(undefined);
    if (!yes) { field.focus(); return; }
    const sequence = recorded();
    for (const other of others) setKeys(other, keys(other).filter((key) => !overlaps(key, sequence)));
    apply();
  };

  return (
    <>
      <dialog class="dlg kb-edit" ref={dialog.ref} aria-labelledby="kb-edit-title"
        onCancel={(e) => { e.preventDefault(); close(); }}>
        <div class="dlg-head"><h2 id="kb-edit-title">{tr("Change shortcut")}</h2></div>
        <div class="dlg-body">
          <p>{tr("Press the new shortcut for %1.", title)}</p>
          <div class="kb-capture-row">
            <div ref={field} class="dlg-input kb-capture" tabindex="0" role="textbox" aria-label={tr("Shortcut for %1", title)}
              data-shortcut-context="capture" onKeyDown={record} onKeyUp={released} onBlur={finish}>
              <Show when={recorded().length} fallback={<span class="kb-placeholder">{tr("Press shortcut")}</span>}>
                {nativeText(recorded())}{recording() ? ", …" : ""}
              </Show>
            </div>
            <button type="button" class="btn" onClick={() => { finish(); setRecorded([]); setReason(""); field.focus(); }}>{tr("Clear")}</button>
          </div>
          <Show when={reason()}><div class="dlg-error">{reason()}</div></Show>
        </div>
        <div class="dlg-foot">
          <span class="spacer" />
          <button type="button" class="btn" onClick={close}>{tr("Cancel")}</button>
          <button type="button" class="btn primary" onClick={accept}>OK</button>
        </div>
      </dialog>
      <Show when={asking()}>
        {(others) => (
          <Question title={tr("Shortcut in use")} text={tr("Already used by %1. Reassign?", separatedList(others().map((id) => tr(command(id)?.title ?? id))))} answer={reassign} />
        )}
      </Show>
    </>
  );
}
