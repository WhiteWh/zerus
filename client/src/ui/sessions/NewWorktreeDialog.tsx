import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { CreatedWorktree } from "../../types";
import { Icon } from "../lib/icons";
import { composing } from "../lib/keys";
import type { Json } from "../lib/json";
import { FolderBrowserDialog } from "../components/FolderBrowser";
import type { FolderBrowserCalls } from "../components/folder-browser";
import type { Style } from "./paths";
import { NewWorktree, baseKey } from "./worktree-dialogs";
import { tr } from "../../i18n";

// Create manually… (NewSessionDialog::createWorktree 1125-1143): Qt's
// NewWorktreeDialog, from the prototype's layout (design/webui/src/dialogs/
// setup/NewSession.tsx, NewWorktreeForm) where Qt does not differ. Its rules
// are worktree-dialogs.ts's. Create worktree is the dialog's default button
// and the one call here that creates; while it runs every control is off and
// closing is refused. The checkout stays if the session never starts.

/**
 * Start from: Qt's editable combo box. Typing edits the base; with the list
 * closed the arrows and Page keys step from the current item (`baseKey`),
 * Alt+Down and F4 open it; with the list open the arrows move, Enter picks
 * and Escape closes the list only.
 */
function BaseField(props: {
  value: string; options: string[]; current: number; disabled: boolean; open: boolean;
  onOpen(open: boolean): void; onInput(text: string): void; onPick(index: number): void; onStep(by: 1 | -1): void;
}) {
  let root!: HTMLDivElement;
  const [active, setActive] = createSignal<number>(-1);
  const show = () => { setActive(props.current); props.onOpen(true); };
  const pick = (index: number) => {
    props.onOpen(false);
    if (index >= 0 && index < props.options.length) props.onPick(index);
  };
  const keys = (e: KeyboardEvent) => {
    if (props.disabled || composing(e)) return;
    const count = props.options.length;
    if (props.open) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const at = active();
        setActive(Math.max(0, Math.min(count - 1, at < 0 ? 0 : at + (e.key === "ArrowDown" ? 1 : -1))));
      } else if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); pick(active()); }
      else if (e.key === "Tab") props.onOpen(false);
      return;
    }
    switch (baseKey(e)) {
      case "open": e.preventDefault(); show(); return;
      case "previous": e.preventDefault(); props.onStep(-1); return;
      case "next": e.preventDefault(); props.onStep(1); return;
    }
  };
  createEffect(() => {
    if (!props.open) return;
    const away = (e: PointerEvent) => { if (!root.contains(e.target as Node)) props.onOpen(false); };
    document.addEventListener("pointerdown", away, true);
    onCleanup(() => document.removeEventListener("pointerdown", away, true));
  });
  return (
    <div class="nw-base" ref={root}>
      <input id="nw-base" class="field mono grow" role="combobox" aria-expanded={props.open} aria-controls="nw-base-list" aria-autocomplete="none"
        aria-activedescendant={props.open && active() >= 0 ? `nw-base-o-${active()}` : undefined}
        title={tr("Local branch, tag, commit or an already fetched remote branch (for example origin/main).")} value={props.value} disabled={props.disabled}
        onInput={(e) => props.onInput(e.currentTarget.value)} onKeyDown={keys} />
      <button type="button" class="icon-btn nw-base-open" tabindex="-1" aria-label={tr("Start from")} disabled={props.disabled}
        onClick={() => (props.open ? props.onOpen(false) : show())}><Icon name="chevron-down" size={14} /></button>
      <Show when={props.open}>
        <ul class="ns-choice-list nw-base-list" id="nw-base-list" role="listbox" aria-label={tr("Start from")}>
          <For each={props.options}>{(option, index) => (
            <li id={`nw-base-o-${index()}`} role="option" aria-selected={option === props.value} classList={{ on: option === props.value, active: index() === active() }}
              onPointerEnter={() => setActive(index())} onMouseDown={(e) => e.preventDefault()} onClick={() => pick(index())}>
              <span class="mono ell">{option}</span>
            </li>
          )}</For>
        </ul>
      </Show>
    </div>
  );
}

export function NewWorktreeDialog(props: {
  /** "" for this computer, else the alias. */
  target: string;
  /** The computer as the dialog names it. */
  machine: string;
  source: string;
  catalog: Json;
  branch: string;
  style: Style;
  create(branch: string, base: string, destination: string): Promise<CreatedWorktree>;
  /** This computer's folder dialog for Parent folder…; a peer's opens the folder browser. */
  pickLocal(start: string): Promise<string | null>;
  browse: FolderBrowserCalls;
  onCreated(created: CreatedWorktree): void;
  onClose(): void;
}) {
  let el!: HTMLDialogElement;
  let branch!: HTMLInputElement;
  let gone = false;
  const form = new NewWorktree(props.create, props.machine, props.source, props.catalog, props.branch, props.style);
  const [version, setVersion] = createSignal(0);
  form.onChange = () => setVersion((v) => v + 1);
  form.onCreated = (created) => {
    if (gone) return;
    props.onCreated(created);
    props.onClose();
  };
  const v = createMemo(() => { version(); return form.view(); });
  const [baseOpen, setBaseOpen] = createSignal(false);
  const [browsing, setBrowsing] = createSignal<string>();

  onMount(() => {
    el.showModal();
    queueMicrotask(() => { branch.focus(); branch.select(); });
  });
  onCleanup(() => {
    gone = true;
    if (el.open) el.close();
  });

  const close = () => { if (form.canClose()) props.onClose(); };
  const parentFolder = async () => {
    if (v().busy) return;
    const start = form.browseStart();
    if (props.target !== "") { setBrowsing(start); return; }
    let chosen: string | null = null;
    try { chosen = await props.pickLocal(start); } catch { chosen = null; }
    if (!gone && chosen) form.browsed(chosen);
  };
  // Create worktree is the default button: Enter presses it, except on Cancel, which Enter presses itself.
  const keys = (e: KeyboardEvent) => {
    if (e.key !== "Enter" || composing(e) || e.defaultPrevented) return;
    const target = e.target as HTMLElement;
    if (target.dataset.role === "cancel") return;
    e.preventDefault();
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    form.create();
  };

  return (
    <>
      <dialog class="dlg nw-dlg" ref={el} aria-labelledby="nw-title" onKeyDown={keys}
        onCancel={(e) => {
          e.preventDefault();
          if (baseOpen()) { setBaseOpen(false); return; }
          close();
        }}
        onClose={() => {
          if (gone) return;
          // A close that `cancel` could not refuse (WebView2's close watcher closes a modal on a
          // second Escape without user activation): while the worktree is being created the
          // dialog shows again and keeps its place, so the answer still arrives.
          if (!form.canClose()) { el.showModal(); return; }
          props.onClose();
        }}>
        <div class="dlg-head"><h2 id="nw-title">{tr("New worktree")}</h2></div>
        <div class="dlg-body nw-body">
          <p class="nw-context">{v().context}</p>
          <div class="form-row">
            <label for="nw-branch">{tr("New branch")}</label>
            <input id="nw-branch" ref={branch} class="field mono" placeholder="feature/my-task" value={v().branch} disabled={v().busy}
              onInput={(e) => form.setBranch(e.currentTarget.value)} />
          </div>
          <div class="form-row">
            <label for="nw-base">{tr("Start from")}</label>
            <BaseField value={v().base} options={v().bases} current={v().baseIndex} disabled={v().busy} open={baseOpen()} onOpen={setBaseOpen}
              onInput={(text) => form.setBase(text)} onPick={(index) => form.chooseBase(index)} onStep={(by) => form.stepBase(by)} />
          </div>
          <div class="form-row">
            <label for="nw-destination">{tr("New folder")}</label>
            <div class="row">
              <input id="nw-destination" class="field mono grow" value={v().destination} disabled={v().busy} onInput={(e) => form.setDestination(e.currentTarget.value)} />
              <button type="button" class="btn" disabled={v().busy} onClick={() => void parentFolder()}>{tr("Parent folder…")}</button>
            </div>
          </div>
          <p class="nw-help">{tr("Creates a new branch and working folder. It remains on this computer if you cancel or cannot start the session.")}</p>
          <p class="nw-error" role="status">{v().error}</p>
        </div>
        <div class="dlg-foot">
          <span class="spacer" />
          <button type="button" class="btn" data-role="cancel" disabled={v().busy} onClick={close}>{tr("Cancel")}</button>
          <button type="button" class="btn primary" disabled={!v().createEnabled} onClick={() => form.create()}>{tr("Create worktree")}</button>
        </div>
      </dialog>
      <Show when={browsing()}>{(start) => (
        <FolderBrowserDialog computer={props.machine} start={start()} calls={props.browse}
          onChoose={(path) => form.browsed(path)} onClose={() => setBrowsing(undefined)} />
      )}</Show>
    </>
  );
}
